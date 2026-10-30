# Legacy setting aliases

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

This slice lets an unchanged fake-gcs-server deployment start after only the
image name changes. It extends UX-003; the row stays `PARTIAL`.

## Behavior

`serve` translates the known fake-gcs-server flags (one or two hyphens) and
`FAKE_GCS_*` variables into native settings. Each legacy input produces one
warning that names its native replacement. As in fake-gcs-server, a flag wins
over a variable, and an empty variable counts as unset.

| fake-gcs-server | Native setting |
| --- | --- |
| `scheme` | `scheme` |
| `host`, `port` | `listen` = `HOST:PORT` (defaults `0.0.0.0` and `4443`; `localhost` becomes `127.0.0.1`) |
| `port-http` | `listen_http` = `HOST:PORT` |
| `external-url` | `api_base_url` |
| `public-host` | `public_base_url` = `http://` with scheme `http`, otherwise `https://`, followed by the value |
| `data` | `seed_dir` |
| `cert-location`, `private-key-location` | `tls_cert`, `tls_key` |
| `cors-headers` | `cors_fallback` = `allow-all` |
| `location` | `default_location` |
| `log-level` | `log_level`; logrus `warning`, `trace`, `fatal` and `panic` become `warn`, `debug`, `error` and `error` |
| `backend` `filesystem` or `memory` | No setting; warns. Memory data survives restarts in `data_dir`. |
| `filesystem-root` | No setting; warns to mount the volume at `data_dir` and to convert old data with `gcs-mimic import fake-gcs-server` |
| `event.*` | Error: startup stops |
| Any other `backend` value | Error: startup stops |

A translated value is validated as the native setting, so an invalid value
fails with the native message keyed by the legacy name. Values are quoted in
advice only where existing diagnostics already allow it.

## Design choices

- **Events still fail.** A test that waits for a notification would otherwise
  start and time out without a visible cause.
- **`filesystem-root` is not a data directory.** A reused fake-gcs-server
  volume is owned by root, while the image runs as UID 10001. Writing there
  would stop startup, which this slice exists to avoid.
- **Within one source, a translation wins over a native value** and warns that
  it overrides it. The image sets `GCS_MIMIC_LISTEN` itself, so preferring the
  native value would silently ignore an existing `FAKE_GCS_PORT`. A higher
  source still wins: a native flag overrides a legacy variable.
- **A leading legacy flag selects `serve`.** A fake-gcs-server Compose
  `command` replaces the image's `serve`. Native options without `serve` still
  fail with the existing advice.
- **The healthcheck falls back to PID 1's options.** A Compose `command`
  reaches only the server process. `gcs-mimic healthcheck` first probes the URL
  derived from the environment and file, as before. Only if that fails, and PID
  1 is `gcs-mimic` running `serve` or a leading legacy flag, does it derive a
  second URL from those options and probe it. Reading PID 1 only after a
  failure matters on Apple Silicon: Rosetta emulates `/proc/<pid>/cmdline` for
  an amd64 image with a temporary file, so reading it on a read-only root
  crashes the probe. Reading it unconditionally made every read-only amd64
  container unhealthy there; now only a legacy `command` on a read-only amd64
  container under Rosetta still fails, which ordinary setups do not use.
- **Helper commands still reject legacy names.** They have no server settings.

## Acceptance and verification

| Case | Test |
| --- | --- |
| Every known legacy name in variable and flag forms warns or, for events and unknown backends, fails, without printing values | `config_input::tests::legacy_inventory_and_all_flag_forms_are_diagnosed_even_when_empty` |
| A typical fake-gcs-server command produces the expected native configuration, with a warning per flag | `config_input::tests::fake_gcs_server_compose_command_starts_with_translated_settings` |
| Source precedence, the override warning, memory backend and event rejection | `config_input::tests::legacy_aliases_follow_source_precedence_and_keep_rejecting_events` |
| The binary started with fake-gcs-server flags and no `serve` becomes ready and ignores `filesystem-root` | `fake_gcs_server_command_without_serve_starts_with_translated_settings` |
| Advice, redaction, dual listeners, location, encoding and limits | the existing `config_input`, `config`, `foundation`, `log_level` and `bucket_initialization` tests, updated for warnings |

Results on 2026-10-30:

- `cargo test --lib config`: 43 passed.
- `cargo test --test foundation`: 28 passed;
  `--test log_level`: 10 passed; `--test bucket_initialization`: 3 passed.
- `cargo test --workspace --locked`: 1348 passed, 0 failed.
- `cargo xtask lint`: 0 production violations.
- A locally built image ran two Compose services with only fake-gcs-server
  settings: one with `command: [-scheme, http, -host, 0.0.0.0, -port, 4443,
  -external-url, ..., -backend, filesystem, -filesystem-root, /storage, -data,
  /data]`, one with `FAKE_GCS_SCHEME=http`, `FAKE_GCS_PORT=8080` and
  `FAKE_GCS_BACKEND=memory`. `docker compose up --wait` reported both healthy.
  Bucket creation, upload and download passed on both, the `/data` seed was
  readable, and the resumable `Location` used the external URL. A container
  with `-event.pubsub-topic` reported the event error and did not start.

Not covered: legacy Host matching from `public-host` still requires
`GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`, as before.
