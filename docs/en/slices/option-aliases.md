# Option aliases

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

This slice lets a command line written for fake-gcs-server start gcs-mimic after
only the image name changes. It extends UX-003; the row stays `PARTIAL`.

## Behavior

`serve` translates the flags below into native settings. A flag warns only when
the translation leaves something the user needs to know. The last occurrence of
a repeated flag wins.

Every server option, native or translated, takes one or two hyphens and a value
after a space or `=`. A command line whose first argument is an option runs
`serve`. These forms need 0.15.0 or later.

| Flag | Native setting | Startup notice |
| --- | --- | --- |
| `host`, `port` | `listen`; a lone flag keeps the other part of the effective address (`localhost` becomes `127.0.0.1`) | None for a valid value |
| `port-http` | `listen_http`; keeps its IP, or else the IP of `listen` | Ignored with a warning unless `scheme=both` |
| `external-url` | `api_base_url` | None for a safe URL |
| `public-host` | `public_base_url` = `http://` with scheme `http`, otherwise `https://`, followed by the value | Warns: Host matching needs the legacy profile |
| `data` | `seed_dir` | None |
| `cert-location`, `private-key-location` | `tls_cert`, `tls_key` | None |
| `cors-headers` | `cors_fallback` = `allow-all` | Warns: remove it |
| `location` | `default_location` | None for a supported location |
| `backend filesystem` | `backend` = `disk` | None |
| `filesystem-root` | No setting | Warns: mount the volume at `data_dir` and convert old data with `gcs-mimic import fake-gcs-server` |
| `event.*` | The global notification rule, delivered to an explicit local Pub/Sub emulator | Warns: only local delivery is enabled |

`scheme`, `log-level` and the other `backend` values are native options, so
`-scheme` and `--scheme` are the same option. A translated value is validated
as the native setting, so an invalid value fails with the native message keyed
by the flag as typed. Values are quoted in advice only where existing
diagnostics already allow it.

Since 0.15.0, `FAKE_GCS_*` variables are not read. Earlier versions
translated them as well.

## Design choices

- **Incomplete events fail.** Event delivery requires project/topic and an explicit
  local Pub/Sub emulator; they never silently fall back to Google or discard events.
- **`filesystem-root` is not a data directory.** A reused fake-gcs-server
  volume is owned by root, while the image runs as UID 10001. Writing there
  would stop startup, which this slice exists to avoid.
- **A lone port or host keeps the rest of the address.** The image's
  `GCS_MIMIC_LISTEN` stays on `0.0.0.0` and a native binary stays on loopback,
  where an unauthenticated emulator belongs. A malformed configured address is
  reported under its own name instead of being replaced.
- **On one command line, a translation wins over a native option** and warns
  that it overrides it. A higher source still wins over a lower one.
- **The first argument selects `serve`.** A Compose `command` replaces the
  image's `serve`, and every other command starts with a word.
- **The healthcheck falls back to PID 1's options.** A Compose `command`
  reaches only the server process. `gcs-mimic healthcheck` first probes the URL
  derived from the environment and file, as before. Only if that fails, and PID
  1 is `gcs-mimic` running server options, does it derive a second URL from
  those options and probe it. Reading PID 1 only after a failure matters on
  Apple Silicon: Rosetta emulates `/proc/<pid>/cmdline` for an amd64 image with
  a temporary file, so reading it on a read-only root crashes the probe. Now
  only a server-option `command` on a read-only amd64 container under Rosetta
  still fails, which ordinary setups do not use.
- **Helper commands still reject these flags.** They have no server settings,
  so the error names the setting for `serve`.

## Acceptance and verification

| Case | Test |
| --- | --- |
| Every known flag in all four forms is recognized without printing values | `config_input::tests::alias_inventory_and_all_flag_forms_are_diagnosed_even_when_empty` |
| A typical command produces the expected native configuration and warns only where something differs | `config_input::tests::fake_gcs_server_compose_command_starts_with_translated_settings`, `config_input::tests::only_inputs_that_behave_differently_warn` |
| One and two hyphens, separate and equals values mean the same for every server option | `config_input::tests::one_and_two_hyphens_mean_the_same_for_every_server_option`, `config_input::tests::option_order_and_value_forms_do_not_change_the_configuration` |
| A first argument that is an option starts the server; help spellings print usage | `config_input::tests::option_first_command_lines_start_the_server`, `config_input::tests::help_is_recognized_only_at_option_positions`, `native_options_without_serve_start_quietly_on_loopback`, `every_help_spelling_prints_usage_without_starting_the_server` |
| A lone port or host keeps the rest of the address; a malformed one is reported | `config_input::tests::a_lone_port_or_host_keeps_the_rest_of_the_listener_address` |
| `port-http` outside `both` is ignored with a warning, and native `listen_http` is rejected | `config_input::tests::a_port_http_alias_is_ignored_outside_both_but_a_native_listen_http_is_rejected` |
| `FAKE_GCS_*` variables have no effect | `config_input::tests::fake_gcs_server_variables_are_not_read`, `fake_gcs_server_variables_do_not_affect_the_server` |
| The binary started without `serve` becomes ready and ignores `filesystem-root` | `fake_gcs_server_command_without_serve_starts_with_translated_settings` |

Results on 2026-11-09: `bash scripts/test-rust.sh` passed every server test
binary, and `cargo xtask lint` found 0 production violations. A locally built
arm64 image started with 30 option forms, including both hyphen counts, both
value forms, either order and the help and version spellings. Each behaved as
above, and the image healthcheck passed on every form that keeps serving.
Bucket creation, upload and download passed with `--scheme http --port 9000`.

Not covered: Host matching from `public-host` still requires
`GCS_MIMIC_COMPAT_PROFILE=legacy-emulator`.
