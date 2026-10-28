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

Use native names by role. **gcs-mimic rejects all 19 known
`FAKE_GCS_*` variables**, including empty values, with migration instructions;
they are not aliases. Unknown `FAKE_GCS_*` names warn without stopping startup.
Unknown `GCS_MIMIC_*` names fail validation, and an empty native value does not
generally mean “use the default.” See the
[native configuration reference](getting-started.md#native-binary-and-configuration).

| Source role | Current native setting | Remaining difference |
| --- | --- | --- |
| Scheme | `GCS_MIMIC_SCHEME` | `http`, `https` or `both`; primary HTTPS in both mode |
| Bind host and port | `GCS_MIMIC_LISTEN` | One `IP:port` value |
| Second HTTP listener | `GCS_MIMIC_LISTEN_HTTP` | Requires `scheme=both`; defaults to primary IP:8000 |
| Advertised API endpoint | `GCS_MIMIC_API_BASE_URL` | API origin, separate from public URL construction |
| Seed input | `GCS_MIMIC_SEED_DIR`, `GCS_MIMIC_SEED_PROJECT` | One-time import; MIME and metadata differ |
| Persistent destination | `GCS_MIMIC_DATA_DIR` | SQLite plus opaque files; cannot mount legacy `/storage` directly |
| Supplied certificate/key | `GCS_MIMIC_TLS_CERT`, `GCS_MIMIC_TLS_KEY` | Supply a matching pair |
| Public-host behavior | `GCS_MIMIC_PUBLIC_BASE_URL` | Origin with DNS-domain routing; legacy Host matching is opt-in. Normalized default ports, IP virtual disabling and authority conflicts retain documented local boundaries. |
| Bucket CORS | JSON bucket API | Per-bucket XML rules; separate fixed JSON defaults |
| Bucket location | JSON bucket API; `GCS_MIMIC_DEFAULT_LOCATION` / `--default-location` / JSON `default_location` | Native `US` default; case-insensitive official regional, predefined dual-region and `US`, `EU`, `ASIA` names; uppercase responses with the matching locationType. Explicit nonempty API/initializer location wins; null/empty uses the default; existing buckets stay unchanged. [Supported names and limits](slices/sdk-regional-locations.md). |
| Log level | `GCS_MIMIC_LOG_LEVEL`, `--log-level`, JSON `log_level` | Structured stderr minimum severity; debug/info/warn/error, warning aliases warn; validation diagnostics and stdout ready/stopped are unfiltered |
| Memory backend, event publisher | No current equivalent | Missing capabilities remain on the roadmap |

The legacy profile rejects explicit `default_location`. Its create API accepts
only case-insensitive `US-CENTRAL1` when location is explicit, stores `US` and
returns fixed `US-CENTRAL1`; it does not implement configurable regional placement.
A public URL helper does not establish `-public-host` parity.
See [option examples](fake-gcs-server-migration.md#startup-options-and-current-equivalents).

`GCS_MIMIC_PORT` in the repository's source-build Compose file is a **Compose
host-port variable**, not a server environment setting. Passing it to the binary
does not configure the listener. Do not mechanically rename every `FAKE_GCS_`
suffix to `GCS_MIMIC_`.

## Migration diagnostics

Startup collects independent CLI, environment, JSON, TLS and identity failures
before opening the catalog or binding a listener. It emits newline-delimited
JSON on stderr: warnings first, then errors, each with a `diagnostics` array of
`source`, original `key`, and actionable `fix`. Errors return nonzero with no
ready event or data-directory creation. Diagnostic records sort by source and
original key; repeated keys keep encounter order. Values and material contents
are omitted, except validated migration host/port/scheme/location/log-level
values and HTTP(S) URLs without credentials, queries or fragments.
Unknown legacy environment values are not decoded, including non-UTF-8 values.
Invalid effective native/known legacy encodings join the same error batch;
they do not hide independent CLI or material errors.

For example, `FAKE_GCS_HOST=127.0.0.2` with `FAKE_GCS_PORT=8080` recommends
`GCS_MIMIC_LISTEN=127.0.0.2:8080`. A safe external URL retains its proxy prefix.
Public-host advice supplies `GCS_MIMIC_PUBLIC_BASE_URL` and the opt-in legacy
profile; native path-style Host acceptance stays unrestricted.
`FAKE_GCS_LOCATION` recommends the corresponding native default with an uppercase
supported name, for example `GCS_MIMIC_DEFAULT_LOCATION=US-CENTRAL1` or
`GCS_MIMIC_DEFAULT_LOCATION=ASIA-NORTHEAST1`, together with
`GCS_MIMIC_COMPAT_PROFILE=native`. Unknown names are redacted and receive valid
examples. The legacy profile's global default remains unsupported.
Unsupported log settings,
memory backend and event delivery are identified as unsupported; future names
are not working replacements. JSON CORS callers can remove the old global
header setting; XML/public callers configure bucket CORS `responseHeader`.

Legacy option names are detected in one- and two-hyphen forms with separate or
equals values, including a Compose command that omits `serve`. Native
`serve --scheme http` remains valid; native options require `--name VALUE`,
so `--scheme=http` receives syntax advice. Native value positions such as
`url public --object -data` retain their meaning. Compose `command` replaces
the image CMD: include `serve` when supplying server arguments. The image's
`GCS_MIMIC_LISTEN=0.0.0.0:4443` and `GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`
environment defaults retain network access and persistence; override these values
to change them while keeping the automatic healthcheck on the same listener.

`GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE` and `GCS_MIMIC_TLS_DIR` are Compose-only
interpolation variables; keep them outside the native server environment.
Use `LISTEN` for server binding and `TLS_CERT`/`TLS_KEY` for actual PEM files.
With no `seed_dir`, an unrelated nonempty `/data` warns that
`GCS_MIMIC_SEED_DIR=/data` imports once into a fresh data directory. No implicit
import occurs. Overlap, ancestors and symlink aliases of native storage suppress
that advice; inspection uncertainty warns without recommending overlapping seed.

Configuration accepts at most 1024 supplied settings across CLI/environment/file
and keys at most 256 UTF-8 bytes; duplicate JSON keys count as occurrences.
The JSON file retains its 64 KiB limit. See the
[workflow contract](slices/configuration-workflows.md#shared-diagnostics-contract)
and [execution evidence](evidence/configuration-diagnostics.md).

## Optional gRPC listener

The optional native `grpc_listen` setting has no legacy flag alias. Set
`--grpc-listen 127.0.0.1:4444`, `GCS_MIMIC_GRPC_LISTEN=127.0.0.1:4444`, or JSON
`{"grpc_listen":"127.0.0.1:4444"}` to open a separate plaintext h2c socket.
It is disabled by default and uses the normal CLI > environment > file precedence.
Go's `STORAGE_EMULATOR_HOST_GRPC=127.0.0.1:4444` selects this listener.
Storage v2 supports scoped bucket CRUD/updates/IAM/retention locking, object
reads/writes/list/delete/restore, resumable uploads including
CancelResumableWrite, rewrite/compose/move and bidirectional ranged reads.
Rapid append, unsupported fields and unknown RPCs return non-retryable
`UNIMPLEMENTED` with ErrorInfo and JSON API guidance. The same endpoint serves
the reference's twelve legacy Storage v1 RPCs, including project-free bucket
listing; see the [legacy contract](slices/grpc-v1-legacy.md). The
[gRPC contract](slices/grpc-v2-foundation.md) records bounds and verification.

## Public origin

`--public-base-url` / `GCS_MIMIC_PUBLIC_BASE_URL` / JSON `public_base_url`
accepts an HTTP(S) origin with optional port and root path only. DNS is
lowercased, trailing root slashes and scheme-default ports are removed; IPv6
stays bracketed. The DNS hostname joins the XML base-domain list, merging an
existing entry within a total limit of 32; duplicates within the explicit list
remain errors. IP origins warn and disable only public-derived virtual hosts.

With `compat_profile=legacy-emulator`, this origin enables the measured public
Host matching and complete-hostname fallback. Native path-style accepts any
Host. Keep API/public authorities equal for SDK XML reads in legacy mode.
The [Host contract](slices/configuration-workflows.md#host-matching-measured-reference-and-normative-rules)
defines port, namespace, explicit-domain and signature boundaries.
URL helpers read the file's public origin only with explicit `--config PATH`;
`--base-url` overrides it. They ignore implicit environment/config selection and
server-only semantics, while validating the explicit file's syntax, flat types,
64 KiB size and public origin even when overridden. No URL grants permissions.
DESIGN-055 and UX-003 remain PARTIAL. See the
[public-origin evidence](evidence/configuration-public-origin.md).

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
