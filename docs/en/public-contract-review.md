# Public contract review for the 1.0.0 freeze

This document supports step 1 of the [1.0.0 gate](release-policy.md#the-100-gate), "Freeze the public contract". It inventories the stable surface and proposes changes for the project owner to decide. The owner has decided candidates 1, 2, 4, 5, 6 and 7, and the `scheme` and `data_dir` rows of candidate 3. Candidates 1, 2, 4, 5 and 7 and the `data_dir` row of candidate 3 are implemented in `0.18.0`. Candidate 6 needs no code change; its document changes remain. The `scheme` row is a pending design for the next release. Each decided candidate lists what it changed or still needs, and the points it leaves open. The remaining rows of candidate 3 and candidates 8 to 17 are undecided proposals.

While the project is in `0.x`, a minor release may make breaking changes, so the breaking changes decided here ship in `0.18.0` without a transition period. The `scheme` default is the exception: it changes once, in the next minor release, together with the capability it depends on (candidate 3). The deprecation procedure for `1.x` is decided at the freeze.

Reviewed source: version `0.17.1`, commit `a53a1f8c`, on 2026-11-10. The decisions of 2026-11-11 were checked against version `0.17.1`, commit `10266125`. The inventory rows that `0.18.0` changes (environment variables, alias flags, catalog schema and ready event) were reconciled with the `0.18.0` source on 2026-11-11; the other rows still describe the reviewed source.

Lifecycle Delete execution uses the `lifecycle_execution` option below. It defaults to disabled with directory synchronization and enabled otherwise; an explicit choice overrides that default.

## How to read this document

Every claim about behavior comes from reading the code. The behaviors marked "observed" were also run against a debug build of the reviewed source. The supported API subset is not repeated here, because [`compatibility/features.json`](../../compatibility/features.json) is its only source.

Document abbreviations used in the tables:

| Abbreviation | Document |
| --- | --- |
| H | `--help` text, built by `help()` in `src/config/help.rs` (the `HELP` constant in `src/main.rs` on the reviewed source) |
| L | `llms.txt` |
| N | `docs/en/native-binary.md` |
| C | `docs/en/container-usage.md` |
| F | `docs/en/fake-gcs-server-configuration.md` |
| R | `README.md` |
| S | The named file under `docs/en/slices/` |

The "Agree" column says whether code and documents agree. A number refers to the [list of disagreements](#disagreements-between-code-and-documents).

The inventory and the "Reason" and "Cost" entries of each candidate describe the current code, not the decided outcome. A decided candidate adds these entries:

- **Owner decisions:** what the owner decided, with the date.
- **0.x defaults:** choices made under the owner's direction where the decision left a detail open. They hold for `0.x` and can be revisited before the freeze.
- **Required change:** what must change before the freeze. Files are named as found on the checked source; nothing listed there has been changed yet. Items marked **Breaking** change behavior that users can rely on today, so they must ship in a `0.x` minor release before the first release candidate. Other items are additive or affect only documents and tests.
- **Open points:** what the decision leaves undecided, including questions found while checking the code.
- **Implemented scope** and **Verification limit:** what has shipped and what remains unverified, once a candidate is implemented.

## Stable surface inventory

### Commands

| Command | Defined in | Documented in | Agree |
| --- | --- | --- | --- |
| `gcs-mimic serve [OPTIONS]` | `src/main.rs` `serve`; `src/config_input.rs` `server_arguments` | H, N, C, S `option-aliases.md` | Yes |
| `gcs-mimic OPTIONS`, when the first argument starts with `-` (runs `serve`) | `src/config_input.rs` `server_arguments` | H, L, S `option-aliases.md` | Yes |
| `--help`, `-h`, `-help`, `--h`, after any server options | `src/config_input.rs` `Inputs::cli`, `requests_help` | H, N, release notes 0.15.0 | 13 |
| `--version`, `-version`, only as the sole argument | `src/config_input.rs` `requests_version` | H, release notes 0.15.0 | 13 |
| `gcs-mimic healthcheck [--url URL] [--ca-cert PATH]` | `src/main.rs` `healthcheck`, `probe` | H, C, S `option-aliases.md` | 17 |
| `gcs-mimic tls ca` | `src/main.rs` `run` | H, `docs/en/local-https.md` | Yes |
| `gcs-mimic url public`, `gcs-mimic url public --help` | `src/main.rs` `url_public`, `URL_PUBLIC_HELP` | H, N, F | Yes |
| `gcs-mimic url sign`, `gcs-mimic url sign --help` | `src/main.rs` `url_sign`, `URL_SIGN_HELP` | H, L, `docs/en/signed-urls.md` | 12 |
| `gcs-mimic test-signer credentials`, `gcs-mimic test-signer --help` | `src/main.rs` `run`, `src/auth/test_signer.rs` | H, L, C | Yes |
| `gcs-mimic sync status`, `findings`, `deletions`, `conflicts`, `rescan`, `approve`, `resolve`, `wait`; `gcs-mimic sync --help` | `src/sync_cli.rs` | H, S `sync-cli.md`, `docs/ja/sync-cli.md` | 11 |
| `gcs-mimic import fake-gcs-server`, `--help` | `src/import/mod.rs` `cli`, `HELP` | H, S `import-filesystem.md`, `docs/en/migrate-from-fake-gcs-server.md` | Yes |

A bare `gcs-mimic` exits 1 and names `serve`. `gcs-mimic serve --version`, `healthcheck --help`, `tls --help`, `tls ca --help`, `url --help`, `sync status --help` and `test-signer credentials --help` all exit 1 (observed).

### Spelling rules

| Surface | Rule | Defined in |
| --- | --- | --- |
| Server option | `--foo-bar`, `-foo-bar`, `--foo_bar` or `-foo_bar`, because `-` and `_` are interchangeable in the name; value after a space or `=`. `--max_connections 10` starts the server (observed) | `src/config_input.rs` `Inputs::cli` |
| Environment | `GCS_MIMIC_FOO_BAR`; the suffix is lowercased and must match a setting exactly | `src/config_input.rs` `Inputs::environment_os` |
| JSON key | `foo_bar` | `src/config.rs` `read_selected_config` |
| Precedence | CLI, then environment, then JSON file, then default | `src/config.rs` `collect_settings` |
| Duplicates | A repeated native option is an error. A repeated alias flag takes the last value (observed) | `src/config.rs` `collect_settings`; `src/config_input.rs` `translate_aliases` |
| Limits | At most 1024 supplied settings across sources; keys at most 256 bytes | `src/config_input.rs` `Inputs::push` |
| Helper commands | Each has its own grammar; see [helper options](#helper-command-options) | `src/main.rs`, `src/sync_cli.rs`, `src/import/mod.rs` |

Any `GCS_MIMIC_*` name that is not a setting stops `serve` and makes `healthcheck` without `--url` fail. Since `0.18.0`, the seven names withdrawn by candidate 2 stop it with advice that names their `GCSM_*` replacement; see [environment variables outside the option table](#environment-variables-outside-the-option-table).

### Server options

Every name below is declared in `src/config/setting.rs`, and its default is in `Config::default` in `src/config.rs`. The table gives the JSON key; the flag and variable follow the spelling rules. "Code" is the file that parses the value. Exceptions: `config` has no JSON key, and `sync_reinitialize` is accepted only on the command line.

| Setting | Default | Accepted | Code | Documents | Agree |
| --- | --- | --- | --- | --- | --- |
| `config` | none | Path to a JSON file | `src/config.rs` | H, L, N, C, F | Yes |
| `log_level` | `info` | `debug`, `info`, `warn`, `error`; `warning` means `warn` | `src/diagnostics.rs` | H, L, N, C, F, R | Yes |
| `listen` | `127.0.0.1:4443`; image `0.0.0.0:4443` | `IP:PORT`, port 0 allowed | `src/config.rs` | H, L, N, C, F, R | Yes |
| `listen_http` | Primary IP, port 8000, with `scheme=both` only | `IP:PORT`; an error without `scheme=both` | `src/config.rs` | H, L, N, F, R | Yes |
| `grpc_listen` | none | `IP:PORT`, plaintext h2c | `src/config.rs` | H, L, N, C, F, R | Yes |
| `trusted_listen` | none | `IP:PORT`; strict mode only | `src/config.rs` | H, L, N, C, F, R | Yes |
| `unix_socket` | none | Path; needs `scheme=http` and no TCP listener setting; Unix only | `src/config.rs` | H, L, S `unix-http-transport.md` | Yes |
| `scheme` | `https` | `http`, `https`, `both` | `src/config.rs` | H, L, N, C, F, R | Yes |
| `tls_cert` | Embedded development certificate | PEM path, at most 1 MiB; needs `tls_key`; an error with `scheme=http` | `src/config.rs`, `src/tls.rs` | H, L, N, F | Yes |
| `tls_key` | Embedded test key | PEM path, at most 1 MiB; needs `tls_cert`; an error with `scheme=http` | `src/config.rs`, `src/tls.rs` | H, L, N, F | Yes |
| `tls_handshake_timeout_ms` | 5000 | Unsigned integer, 0 disables; an error with `scheme=http` | `src/config.rs` | H, L, N | Yes |
| `data_dir` | `data`; image `/var/lib/gcs-mimic` | Nonempty path | `src/config.rs` | H, L, N, C, F | Yes |
| `backend` | `disk` | `disk`, `memory`, and `filesystem` meaning `disk` | `src/config/storage.rs` | H, L, N, F, R | 4 |
| `memory_max_bytes` | Half of available memory after 32 MiB, 64 MiB to 1 GiB; 1 GiB where memory is not read | 1 to 2^40; needs `backend=memory` | `src/config/storage.rs`, `src/memory_limit.rs` | H, L | 15 |
| `memory_catalog_max_bytes` | 16777216 | 1048576 to 2^40; needs `backend=memory` | `src/config/storage.rs` | H, L | Yes |
| `seed_dir` | none | Nonempty path | `src/config.rs` | H, L, N, C, F, S `seed.md` | 2 |
| `default_seed_dir` | none; image `/data` | Nonempty path | `src/config.rs`, `src/config_input.rs` | H, L, N, C, F | Yes |
| `seed_project` | `local-project` | A valid project | `src/config.rs` | H, L, N, C, F | Yes |
| `seed_content_type` | `extension` | `extension`, `octet-stream` | `src/mime.rs` | H, L, N, C, F | Yes |
| `max_seed_entries` | 0 (unlimited) | 0 to 2^64-1 | `src/config.rs` | H, L, N, F | Yes |
| `max_seed_depth` | 0 (unlimited) | 0 to 1024 | `src/config.rs` | H, L, N, F | Yes |
| `sync_dir` | none | Existing writable directory; disk backend; Linux and macOS | `src/config.rs`, `src/sync/mod.rs` | H, L, N, C | Yes |
| `sync_delete_guard` | `true` | `true`, `false`; needs `sync_dir` | `src/config.rs` | H, L, C | 1 |
| `sync_reinitialize` | none | Catalog ID; command line only; needs `sync_dir` | `src/config.rs` | H, L, S `directory-synchronization.md` | Yes |
| `max_sync_entries` | 0 (unlimited) | 0 to 2^64-1; needs `sync_dir` | `src/config.rs` | H, L, N, F | Yes |
| `max_sync_depth` | 0 (unlimited) | 0 to 1024; needs `sync_dir` | `src/config.rs` | H, L, N, F | Yes |
| `default_location` | `US` | Names in `src/bucket.rs`, case-insensitive; native profile only | `src/config.rs`, `src/bucket.rs` | H, L, C, F | 3 |
| `default_soft_delete_retention_seconds` | 0 (off) | 0 to 3155760000; a positive value needs the native profile | `src/config.rs`, `src/bucket.rs` | H, L, N, C, F | Yes |
| `lifecycle_execution` | `disabled` with `sync_dir`, `enabled` otherwise | `enabled` or `disabled`; explicit enabled also deletes synchronized host files. CLI `--lifecycle-execution`, environment `GCS_MIMIC_LIFECYCLE_EXECUTION`, JSON `lifecycle_execution` | `src/config.rs`, `src/storage/catalog/lifecycle.rs` | H, N, F, S `lifecycle-execution.md` | Yes |
| `buckets_file` | none | Path; not with `buckets` | `src/config/document.rs` | H, L, N, C, F, S `bucket-initialization.md` | Yes |
| `buckets` | none | The document inline; not with `buckets_file` | `src/config/document.rs` | H, L, N, C, F, R, S `bucket-initialization.md` | 7 |
| `buckets_file_mode` | `create` for a file, `reconcile` inline | `create`, `reconcile`; needs a document | `src/config.rs` | H, L, N, C, F | Yes |
| `max_buckets_per_batch` | 0 (unlimited) | 0 to 2^64-1 | `src/config.rs` | H, L, N, F | Yes |
| `api_base_url` | Each request's Host | Absolute HTTP(S) base without credentials, query or fragment | `src/config.rs`, `src/url.rs` | H, L, N, C, F | Yes |
| `public_base_url` | none | HTTP(S) origin | `src/config.rs`, `src/url.rs` | H, L, F | Yes |
| `xml_virtual_host_domains` | none | Up to 32 comma-separated DNS names; empty is allowed | `src/config.rs` | H, L, C, S `xml-virtual-host.md` | Yes |
| `admin_enabled` | `false` | `true`, `false` | `src/config.rs` | H, L, N, C | Yes |
| `admin_principal` | none | Registered identity; needs strict mode and `admin_enabled` | `src/config.rs`, `src/config/auth.rs` | H, L, S `runtime-admin-endpoints.md` | Yes |
| `admin_allowed_hosts` | none | Up to 32 comma-separated DNS names; empty is allowed | `src/config.rs` | H, L, C, F | Yes |
| `trusted_principal` | Built-in `trusted-app@gcs-mimic.invalid` | Registered identity or enabled test signer; needs `trusted_listen` | `src/config.rs`, `src/config/auth.rs` | H, L, N, C, F | Yes |
| `trusted_allowed_hosts` | none | Up to 32 comma-separated DNS names; empty is allowed; needs `trusted_listen` | `src/config.rs` | H, L, N, C, F | 9 |
| `local_creator_email` | `local-user@gcs-mimic.invalid` | Email or empty; a non-empty value needs native permissive mode | `src/config.rs` | H, L, N, C, F | Yes |
| `max_connections` | 1024 | 1 to 1048576 | `src/config.rs` | H, L, N, F | Yes |
| `max_grpc_streams` | 100 | 1 to 65536; needs `grpc_listen` | `src/config.rs` | H, L, N, F | Yes |
| `max_object_bytes` | 5497558138880 | 1 to 5497558138880 | `src/config.rs` | H, L, N, F | Yes |
| `max_resumable_sessions` | 0 (unlimited) | 0 to 4294967295 | `src/config.rs` | H, L, N, F | Yes |
| `max_rewrite_sessions` | 0 (unlimited) | 0 to 4294967295 | `src/config.rs` | H, L, N, F | Yes |
| `worker_threads` | Available CPUs, 2 to 8 | 1 to 1024 | `src/config.rs` | H, L, N | Yes |
| `max_blocking_threads` | 8 | 1 to 1024 | `src/config.rs` | H, L, N | Yes |
| `header_read_timeout_ms` | 30000 | Unsigned integer, 0 disables | `src/config.rs` | H, L, N, F | Yes |
| `body_inactivity_timeout_ms` | 0 (off) | Unsigned integer, 0 disables | `src/config.rs` | H, L, N, C, F | Yes |
| `request_timeout_ms` | 180000 | Unsigned integer, 0 disables | `src/config.rs` | H, L, N, F | Yes |
| `connection_timeout_ms` | none | Sets the three settings above; each named explicitly wins | `src/config.rs` | H, L, N, F | Yes |
| `keep_alive_timeout_ms` | 120000 | Unsigned integer, 0 disables | `src/config.rs` | H, L, N, F | Yes |
| `object_request_timeout_ms` | 0 (off) | Unsigned integer, 0 disables | `src/config.rs` | H, L, N, F | Yes |
| `catalog_wait_timeout_ms` | 120000 | Unsigned integer, 0 disables | `src/config.rs` | H, L, N, F | Yes |
| `shutdown_timeout_ms` | 5000 | Unsigned integer, 0 disables | `src/config.rs` | H, L, N | Yes |
| `auth_mode` | `permissive` | `permissive`, `strict` | `src/auth/mod.rs`, `src/config/auth.rs` | H, L, N, C, R | Yes |
| `compat_profile` | `native` | `native`, `legacy-emulator` | `src/config.rs` | H, L, N, C, F | Yes |
| `cors_fallback` | `allow-all` permissive, `none` strict | `none`, `allow-all` | `src/config.rs` | H, L, N, C, F | Yes |
| `signed_host_port` | `lenient` where the test signer is enabled, else `exact` | `exact`, `lenient` | `src/auth/signature.rs` | H, L, N, C, F | Yes |
| `identity_file` | none | Path, at most 16 MiB; not with `identities` | `src/config/document.rs`, `src/auth/mod.rs` | H, L, N, C, F | Yes |
| `identities` | none | The registry inline; not with `identity_file` | `src/config/document.rs`, `src/auth/mod.rs` | H, L, N, C, F | 7 |
| `signed_urls` | `ignore` permissive, `verify` strict | `ignore`, `verify`; strict rejects `ignore` | `src/auth/mod.rs`, `src/config/auth.rs` | H, L, N, C, F, R | Yes |
| `test_signer` | Enabled with permissive `verify` or when strict identities declare it, else disabled | `enabled`, `disabled` | `src/auth/mod.rs`, `src/config/auth.rs` | H, L, N, C, F | Yes |
| `pubsub_emulator_host` | none | `HOST:PORT` or empty | `src/config/notifications.rs` | H, L, C, F | Yes |
| `event_pubsub_project_id` | empty | Project ID | `src/config/notifications.rs` | H, L, C, S `pubsub-notification-delivery.md` | Yes |
| `event_pubsub_topic` | empty | Topic name | `src/config/notifications.rs` | H, L, C, S `pubsub-notification-delivery.md` | Yes |
| `event_bucket` | empty (all buckets) | Bucket name | `src/config/notifications.rs` | H, L, C, S `pubsub-notification-delivery.md` | Yes |
| `event_object_prefix` | empty (all names) | Object name prefix | `src/config/notifications.rs` | H, L, C, S `pubsub-notification-delivery.md` | Yes |
| `event_list` | `finalize` | `finalize`, `delete`, `metadataUpdate`, `archive`, comma-separated, or `none` | `src/config/notifications.rs` | H, L, S `pubsub-notification-delivery.md` | 8 |

### Alias flags

These flags exist only on the command line, mainly for fake-gcs-server command lines. Since `0.18.0`, `src/config_input.rs` splits them into `NativeFlag`, `AliasFlag` (accepted and translated) and `RefusedFlag`, as decided under candidates 5 and 7; S `option-aliases.md` and F list them. They take one or two hyphens and no underscore form. `healthcheck`, `url`, `tls` and `test-signer` reject them with advice; `sync` and `import` reject them as unknown options.

| Flag | Kind | Becomes or stops with | Startup output | Agree |
| --- | --- | --- | --- | --- |
| `host` | Native | IP of `listen`; `localhost` becomes `127.0.0.1` | None | Yes |
| `port` | Native | Port of `listen` | None | Yes |
| `port-http` | Native | Port of `listen_http`; without `scheme=both`, startup fails | None | Yes |
| `scheme`, `log-level` | Native | The native options of the same name | None | Yes |
| `backend` | Native or accepted | `memory` and `disk` are native; `filesystem` becomes `backend=disk` | Native-equivalent block for `filesystem` | 4 |
| `public-host` | Accepted | `public_base_url` with the effective scheme | Native-equivalent block | Yes |
| `data` | Accepted | `seed_dir` | Native-equivalent block | Yes |
| `cert-location` | Accepted | `tls_cert` | Native-equivalent block | Yes |
| `private-key-location` | Accepted | `tls_key` | Native-equivalent block | Yes |
| `event.pubsub-project-id`, `event.pubsub-topic`, `event.bucket`, `event.object-prefix`, `event.list` | Accepted with a Pub/Sub emulator host, refused without one | The `event_*` setting of the same suffix; `event.list none` is refused | Native-equivalent block, or an error | Yes |
| `filesystem-root` | Refused | `data_dir` for native storage and `gcs-mimic import fake-gcs-server` for source data | Error | Yes |
| `external-url` | Refused | `api_base_url` | Error | Yes |
| `cors-headers` | Refused in every authentication mode | Bucket `cors` rules, or an explicit `cors_fallback=allow-all` | Error | Yes |
| `location` | Refused | `default_location` | Error | Yes |

The native-equivalent block is one `info` diagnostic with key `native_equivalent` and a `native_args` array that holds the native options in full. An error is keyed by the flag as supplied and names the native replacement; every refused flag is reported in one batch, with exit status 1, before a catalog is opened.

### Helper command options

| Command | Options | Grammar | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- | --- |
| `healthcheck` | `--url`, `--ca-cert` | Two hyphens, space-separated. A 2-second deadline; requires HTTP 200 | `src/main.rs` | H, C | 17 |
| `url public` | `--base-url`, `--config`, `--bucket`, `--object`, `--style` | Two hyphens, space-separated | `src/main.rs` | H, F, N | Yes |
| `url sign` | The `url public` options, `--method`, `--expires`, `--signer`, `--private-key`, `--header` (repeats), `--query` (repeats), `--timestamp` | Two hyphens, space-separated | `src/main.rs`, `src/auth/signature.rs` | H, `docs/en/signed-urls.md` | 12 |
| `sync` | `--endpoint`, `--ca-cert`, `--token-file`, `--timeout`, `--timeout-ms`, `--revision`, `--cursor`, `--limit`, `--conflict`, `--recovery`, `--action`, `--expected-generation`, `--target-identity`, `--operation-id`, `--confirm-backup-verified`, `--confirm-discard-without-backup`, `--deletion-set`, `--view` | Two hyphens, space-separated, values 1 to 8192 bytes | `src/sync_cli.rs` | H, S `sync-cli.md` | 11 |
| `import fake-gcs-server` | `--source`, `--data-dir`, `--metadata-mode`, `--source-platform`, `--source-format`, `--max-object-bytes`, `--max-entries`, `--max-depth`, `--dry-run`, `--verify-bodies` | Two hyphens, space-separated; a value cannot start with `--` | `src/import/mod.rs` | H, S `import-filesystem.md` | Yes |

### Healthcheck URL selection

Without `--url`, `healthcheck` chooses its target as follows (`src/main.rs` `healthcheck`, `probe_url`, `container_server_args`; `src/config.rs` `healthcheck_url`). C and S `option-aliases.md` describe it; H does not (disagreement 17).

1. It runs the server's configuration validation over the environment and the selected JSON file. An invalid configuration exits 1 without any probe, which is why an unknown `GCS_MIMIC_*` name fails it.
2. It probes `/_internal/healthcheck` at the configured listener. A wildcard address becomes loopback, `scheme=both` selects the HTTP listener, and every scheme but `https` is probed over plain HTTP.
3. If that probe fails, it reads `/proc/1/cmdline`. When PID 1 is `gcs-mimic` running server options, it validates those options, derives a second URL from them and probes it.

An explicit `--url` skips all three steps. Step 3 is what lets the image's `HEALTHCHECK` pass when a Compose `command` such as `--scheme http` configures the server.

### Environment variables outside the option table

| Variable | Effect | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- |
| `PUBSUB_EMULATOR_HOST` | Same as `GCS_MIMIC_PUBSUB_EMULATOR_HOST`, which wins when both are set | `src/config_input.rs`, `src/config.rs` | H, L, F | Yes |
| `GCSM_SYNC_TOKEN` | Bearer token for `gcs-mimic sync`; `--token-file` wins. The server ignores it | `src/sync_cli.rs` | `sync --help`, S `sync-cli.md` | Yes |
| `GCSM_ENDPOINT` | Address for the client programs in `examples/`. The server ignores it | `examples/*`, for example `examples/go/main.go` | `examples/README.md` and each example's README, `docs/en/getting-started.md` | Yes |
| `GCSM_GRPC_ENDPOINT` | gRPC address for `examples/rust`. The server ignores it | `examples/rust/src/main.rs` | `examples/rust/README.md` | Yes |
| `GCSM_BINARY` | Executable that the Go package starts; the package still removes every `GCS_MIMIC_*` name from the child it starts | `go/fakestorage/server.go` | `go/README.md` | Yes |
| `GCSM_PORT`, `GCSM_IMAGE`, `GCSM_TLS_DIR` | Compose interpolation in `compose*.yaml` and `examples/` | `compose.yaml`, `compose.tls.yaml`, `examples/*/compose.yaml` | N, F | Yes |
| `GCS_MIMIC_ENDPOINT`, `GCS_MIMIC_GRPC_ENDPOINT`, `GCS_MIMIC_SYNC_TOKEN`, `GCS_MIMIC_BINARY`, `GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE`, `GCS_MIMIC_TLS_DIR` | Withdrawn in `0.18.0`. `serve` and `healthcheck` without `--url` stop and name the `GCSM_*` replacement; `gcs-mimic sync` stops on the old token name even when a new token or `--token-file` is given; the Go package and the examples stop on their old name when nothing else supplies the value; the shipped Compose files pass the three Compose names into the server only when set, so it stops | `src/config_input/environment.rs` `RenamedEnvironmentVariable`, `src/sync_cli.rs`, `go/fakestorage/server.go`, `examples/*` | Release notes 0.18.0 | Yes |
| `SQLITE_TMPDIR` | Set by the image; read by SQLite, not by gcs-mimic | `Dockerfile` | C | Yes |

`FAKE_GCS_*` variables are not read (`src/config_input.rs` test `fake_gcs_server_variables_are_not_read`; L, F). `STORAGE_EMULATOR_HOST` and similar variables belong to clients.

On the reviewed source, every `GCS_MIMIC_[A-Z0-9_]+` name in the repository was compared with `Setting`: 117 distinct names, of which 70 are settings. Besides the 7 names in the table above, 4 are examples or test inputs for unknown settings (`FOO_BAR`, `UPPER_SNAKE_CASE`, `NO_SUCH_SETTING`, `UNKNOWN_SETTING`). The other 40 are read only by tests, the SDK harnesses and repository scripts, and are not part of the contract: `ALLOWED_HOSTS`, `BIN`, `BUCKET`, `CA_CERT`, `CHECK_FAIL`, `CHECK_LOG`, `CHECK_NO_TESTS`, `CHECK_OVERLAP`, `CHECK_ROOT`, `DIFF_ENDPOINT`, `DIFF_GRPC_BEARER`, `DIFF_PROJECT`, `DIFF_TOKEN`, `DOCKER_GATEWAY`, `EGRESS_PROXY`, `GO_TEST_RUN`, `IMPORT_CRASH_CATALOG`, `IMPORT_CRASH_COMMITTED`, `LOCAL_STRICT_ENDPOINT`, `PHP_DOCKER_NETWORK`, `PRE_PUSH_TEST_FAIL`, `PRE_PUSH_TEST_SERIAL`, `PRE_PUSH_TEST_SLEEP`, `PRE_PUSH_TEST_STATE`, `REFERENCE_ONLY`, `REFERENCE_REPO`, `ROOT_LOCK_TEST`, `SDK_SCHEME`, `SIGNED_TESTS`, `SOURCE_FIXTURE_DIR`, `STRICT_ENDPOINT`, `STRICT_GRPC_ENDPOINT`, `TEST_BACKEND_LOG`, `TEST_LOCK_DIR`, `TEST_PUBSUB_EMULATOR_HOST`, `TEST_RECOVERY_DIR`, `TEST_REJECTED_WAL_DIR`, `TEST_REJECTED_WAL_FIXTURE`, `TOKEN` and `TRUSTED_ENDPOINT`. Since `0.18.0` these use `GCSM_*`, and `BIN` merged into `GCSM_BINARY` ([verification](evidence/environment-names.md)).

### Configuration file formats

| Format | Version | Fields and limits | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- | --- |
| Server JSON (`config`) | None; a `version` key is an unknown setting (observed) | Flat object of setting keys, at most 64 KiB. Values are strings or unsigned integers. `admin_enabled` and `sync_delete_guard` also take booleans, `default_location` also takes null, and `buckets` and `identities` also take an object. `buckets_file` takes only a string | `src/config.rs` `read_config`, `member_text` | N, S `configuration-workflows.md`, `config/development.json` | 7 |
| Bucket document (`buckets_file`, `buckets`) | Required integer `version: 1` | Top: `version`, `project`, `buckets`. Entry: `name`, `project`, `location`, `storageClass`, `labels`, `cors` (`origin`, `method`, `responseHeader`, `maxAgeSeconds`), `versioning.enabled`, `defaultEventBasedHold`, `iamConfiguration.publicAccessPrevention`, `softDeletePolicy.retentionDurationSeconds`, `iamPolicy.bindings`. Unknown fields and fields such as `lifecycle` and `acl` fail. File at most 1 GiB, or 1 KiB per allowed bucket and at least 32 MiB with a limit; entry at most 16 MiB | `src/bucket_init.rs`, `src/bucket_init/policy.rs` | N, C, S `bucket-initialization.md`, `config/buckets.json` | 7, 10 |
| Identity registry (`identity_file`, `identities`) | None | Top: `identities` (required, nonempty), `public_access_prevention_projects`. Entry: `id`, `tokens`, `project_roles`, `public_keys`, `acl_entity`. At most 16 MiB. With permissive `signed_urls=verify`, only `id` and `public_keys` | `src/auth/mod.rs` | N, C, S `strict-auth.md`, `config/identities.json` | 7 |
| Seed directory | None | Top-level directories are buckets; nested regular files are objects; root files are skipped with `seed_skipped` | `src/seed.rs` | N, C, S `seed.md` | Yes |
| Sync directory | None | `BUCKET/OBJECT` files, plus `.gcs-mimic-sync.json`, `.gcs-mimic-sync-work` and `.gcs-mimic-sync.json.staging` | `src/sync/mod.rs`, `src/sync/mapping.rs` | C, S `directory-synchronization.md` | Yes |
| TLS material | PEM | Certificate chain and key, each at most 1 MiB | `src/tls.rs` | N, `docs/en/local-https.md` | Yes |

### Data directory access contract

| Property | Current behavior | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- |
| Ownership | Exclusive; an advisory lock on `catalog.lock` | `src/storage/lock.rs` | N | Yes |
| Layout | `catalog.sqlite3`, `catalog.lock`, `blobs/`, `staging/` and other internal entries; treated as opaque | `src/storage/catalog/open.rs`, `src/storage/catalog/objects/recovery.rs` | L (opaque), `docs/en/storage-backends.md` | Yes |
| Schema | Version 53 since `0.18.0`; older catalogs migrate forward on open; downgrade is not supported | `src/storage/catalog.rs` `CATALOG_SCHEMA_VERSION` | N, `docs/en/release-policy.md` | Yes |
| Supported upgrades | Data directories from 0.8.0 and later | `tests/it_import_seed/released_data_upgrade.rs`, `tests/fixtures/released-data` | `docs/en/release-policy.md` | Yes |
| Memory backend | Never opens `data_dir` | `src/config/storage.rs`, `src/storage/catalog/open.rs` | H, N | Yes |
| Import destination | Absent or empty, or the same unfinished import | `src/import/mod.rs` | `import fake-gcs-server --help`, S `import-filesystem.md` | Yes |

### Emulator-only routes

All are under `/_internal`, which is outside the Google namespaces. Defined in `src/server.rs` `probe`, `src/server/admin.rs` and `src/server/admin/`.

| Route | Methods | Availability | Documented in | Agree |
| --- | --- | --- | --- | --- |
| `/_internal/health` | GET, HEAD | Always; liveness | H, S `runtime-admin-endpoints.md` | Yes |
| `/_internal/ready`, `/_internal/healthcheck` | GET, HEAD | Always; readiness | H, L, S `runtime-admin-endpoints.md` | Yes |
| `/_internal/config` | PUT, JSON `externalUrl`, `publicHost` | `admin_enabled` | S `runtime-admin-endpoints.md` | 16 |
| `/_internal/objects` | POST, `multipart/related` | `admin_enabled` | S `local-fixture-administration.md` | Yes |
| `/_internal/reset` | POST, body `{"confirm":"gcs-mimic"}` | `admin_enabled` | S `local-instance-reset.md` | 16 |
| `/_internal/delete_all` | POST, empty body | `admin_enabled` | S `local-instance-reset.md` | Yes |
| `/_internal/reseed` | POST or PUT, empty body | `admin_enabled` and `seed_dir` | C, S `atomic-seed-reseed.md` | Yes |
| `/_internal/lifecycle/run` | POST, empty body; counts and effective `enabled` | `admin_enabled`; obeys `lifecycle_execution` | S `lifecycle-execution.md` | Yes |
| `/_internal/sync`, `/_internal/sync/conflicts` | GET | `sync_dir`, not `admin_enabled`; its own control authorization | S `sync-cli.md`, `external-deletion-controls.md` | Yes |
| `/_internal/sync/approve`, `rescan`, `resolve`, `wait` | POST | `sync_dir`, not `admin_enabled`; its own control authorization | S `sync-cli.md`, `external-deletion-controls.md` | Yes |

Management routes refuse browser-initiated requests with 403 and unapproved DNS hosts with 421 (`src/server/admin/guard.rs`; S `http-adapter-boundaries.md`).

### Exit codes and machine-readable output

| Item | Current behavior | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- |
| Exit status | 0 on success, including graceful shutdown on a signal; 1 on any failure | `src/main.rs` `main` | F ("nonzero"), S `sync-cli.md` ("nonzero") | Yes |
| Diagnostics | One JSON line per batch on stderr: `event: "diagnostics"`, `level` (`warn` or `error`), `diagnostics` of `source`, `key`, `fix`; warnings first, sorted by source then key | `src/diagnostics.rs` | N, F | Yes |
| Diagnostic sources | `cli`, `environment`, `config`, `tls`, `identity`, `filesystem`, `catalog`, `initializer`, `seed`, `sync`, `import`, `listener`, `runtime` | `src/diagnostics.rs` | Partly, F | Yes |
| Fix wording | Not stable, by policy | `docs/en/release-policy.md` | `docs/en/release-policy.md` | Yes |
| Ready event | One JSON line on stdout with `event: "ready"`, `executable`, `version`, `healthcheck_path`, `admin_enabled`, effective `lifecycle_execution` (`enabled` or `disabled`), `message`, `listen`, `listeners`, optional `trusted_listen` and `trusted_principal`, `transport`, optional `unix_socket`, with disk storage `data_dir` (absolute, symbolic links not resolved) and `data_dir_state` (`created` or `opened`), the limits and timeouts, `backend`, `auth_mode`, optional `identities`, `test_signer`, `signed_urls`, `seed`, `buckets_file`, `scheme`, `public_base_url`, `public_virtual_host_enabled`, `cors_fallback`, `signed_host_port`, optional `signed_host_port_signers`, `api_base_url`. `protocol` and `gcs_api` were removed in `0.18.0` | `src/startup.rs` `ReadyEvent`, `ReadyDataDirectory` | N, C, S `runtime-admin-endpoints.md`, S `lifecycle-execution.md`, S `startup-phases.md` | Yes |
| Data directory notice | One `info` runtime event `data_dir_created` on stderr with the absolute `data_dir`, only when this start initialized the catalog | `src/startup.rs` | S `startup-phases.md`, N | Yes |
| Stopped event | `{"event":"stopped"}` on stdout | `src/startup.rs` `report_stopped` | N | Yes |
| Runtime events | JSON lines on stderr with `event`, `level` and fields, filtered by `log_level` | `src/diagnostics.rs` `emit` | N | Yes |
| Import output | JSON lines on stdout (`import_page`, `import_complete`); diagnostics on stderr | `src/import/mod.rs`, `src/import/filesystem.rs` | `import fake-gcs-server --help` | Yes |
| Sync output | One JSON document on stdout, also on a non-success HTTP status | `src/sync_cli.rs` | S `sync-cli.md` | Yes |

### Images, tags, paths and archives

| Item | Current value | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- |
| Registries | `ghcr.io/shiroemons/gcs-mimic`, `docker.io/shiroemons/gcs-mimic` (same digests) | `.github/workflows/ghcr.yaml`, `.github/workflows/dockerhub.yaml` | R, L, C, `docs/en/container-distribution.md` | Yes |
| Tags | `X.Y.Z` immutable; `X.Y` and `latest` advance only; no major alias; digest references | `xtask/src/container.rs` `tags`, `scripts/container-release.sh` | `docs/en/container-distribution.md` | 14 |
| Platforms | `linux/amd64`, `linux/arm64` | `.github/workflows/ghcr.yaml` | L, C | Yes |
| Entrypoint and command | `ENTRYPOINT ["gcs-mimic"]`, `CMD ["serve"]` | `Dockerfile` | L, S `option-aliases.md` | Yes |
| Image environment | `PATH=/usr/local/bin`, `GCS_MIMIC_LISTEN=0.0.0.0:4443`, `GCS_MIMIC_DATA_DIR=/var/lib/gcs-mimic`, `GCS_MIMIC_DEFAULT_SEED_DIR=/data`, `SQLITE_TMPDIR=/var/lib/gcs-mimic` | `Dockerfile` | C, F | Yes |
| Paths | `/usr/local/bin/gcs-mimic`, `/var/lib/gcs-mimic`, `/data`, `/usr/share/licenses/gcs-mimic/`, `/tmp`, working directory `/app` | `Dockerfile` | C, L; `/app` is undocumented | Yes |
| User and port | `USER 10001:10001`, `EXPOSE 4443`, `STOPSIGNAL SIGTERM` | `Dockerfile` | L, C | Yes |
| Healthcheck | `gcs-mimic healthcheck`, interval 2 s, timeout 2 s, start period 2 s, retries 15 | `Dockerfile` | C, partly | Yes |
| Native archives | `gcs-mimic-X.Y.Z-<target>` (`.tar.gz` or `.zip` for Windows), eight targets, `SHA256SUMS`, `BUILD-INFO.json` | `.github/workflows/binaries.yaml`, `xtask/src/release.rs` | `docs/en/binary-distribution.md` | Yes |
| Release identity | Git tag `vX.Y.Z`, binary and image `X.Y.Z` | `.github/workflows/binaries.yaml` | `docs/en/release-policy.md` | 14 |

## Disagreements between code and documents

1. Resolved after `0.19.0`: `--sync-delete-guard` help said "deletion ingestion pending" although the code applies the guard during deletion ingestion (`src/sync/deletion.rs`). The help now states the threshold and the approval.
2. Resolved after `0.19.0`: `--seed-dir` help said "image: /data", which the image sets as `GCS_MIMIC_DEFAULT_SEED_DIR`. The help now says that `seed_dir` wins over `default_seed_dir`.
3. Resolved after `0.19.0`: `--default-location` help said `US|EU|ASIA` although the code also accepts every region and dual-region (`src/bucket.rs`). The help now names them.
4. `backend` accepts `filesystem` from the environment and JSON as well as the flag (`src/config/storage.rs`; observed with `GCS_MIMIC_BACKEND=filesystem`). H, N and the error text say `disk` or `memory`. S `option-aliases.md` presents it as a flag translation only, and the helper advice in `alias_advice` says `filesystem` or `memory`.
5. Resolved in `0.18.0`: on the reviewed source the `--cors-headers` warning said "strict mode follows each bucket's cors rules" while the flag set `cors_fallback=allow-all` in strict mode too. The flag now stops startup in every authentication mode; see candidate 7.
6. Resolved in `0.18.0`: on the reviewed source S `option-aliases.md` said `public-host` warned that Host matching needs the legacy profile, which only the helper-command advice did. The per-flag warnings are gone; `public-host` is accepted with the native-equivalent block, and S `option-aliases.md` records its routing difference (candidate 5).
7. Resolved after `0.19.0`: N said `buckets` and `identities` have the "same limits" as their file forms, while in a JSON config file that file's 64 KiB limit binds them (`read_config`). N now says so.
8. Resolved after `0.19.0`: `--event-list` help said `none` disables the global rule, while alone it fails startup, because event filters need a project and topic (`src/config/notifications.rs`). The help now says that any value other than `finalize` needs the topic settings.
9. Resolved after `0.19.0`: C's list of settings that define an empty value omitted `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`, which the code accepts empty beside `trusted_listen`. C and its Japanese companion now list it, and `settings_that_define_an_empty_environment_value_start_with_it` covers it.
10. Resolved after `0.19.0`: L listed the bucket entry's optional fields as `cors`, `iamPolicy`, `versioning` and `softDeletePolicy`, while the code also accepts `project`, `location`, `storageClass`, `labels`, `defaultEventBasedHold` and `iamConfiguration` (`src/bucket_init.rs` `Entry`). L now lists all ten.
11. Resolved after `0.19.0`, except for the duplicate spellings of candidate 9: the main help listed `sync findings` and `sync deletions`, but `sync --help` showed only `status --view`, and the approval error named `--revision` while the help names `--deletion-set`. `sync --help` now shows both forms, and the error names `--deletion-set`. `sync approve` still accepts `--revision`, undocumented.
12. Resolved after `0.19.0`: `url sign --help` listed GET, HEAD, PUT and DELETE, while the code also signs POST with `x-goog-resumable: start` (`src/auth/signature.rs`). The help now lists POST and the header it needs.
13. Resolved after `0.19.0`: the main help's usage line omitted `--h` and `-version`, which the code accepts. It now lists every accepted spelling.
14. Resolved: release tooling accepts `MAJOR.MINOR.PATCH-rc.N` with positive `N`, publishes only its immutable exact container tag and marks its GitHub Release as a non-latest prerelease. Stable versions alone determine floating tags. Each candidate is a supported upgrade source with its own released-data fixture; see candidate 1 and the [release policy](release-policy.md).
15. Resolved after `0.19.0`: `--memory-max-bytes` help and L said half of available memory, while the code subtracts 32 MiB first and uses 1 GiB wherever it cannot read the memory, which is everywhere except Linux (`src/memory_limit.rs`). H, L and N now say so.
16. Resolved after `0.19.0`: S `runtime-admin-endpoints.md` and S `local-instance-reset.md` said management bodies are capped at 64 KiB, while the handlers read at most `RESOURCE_BODY_LIMIT`, which is 16 MiB. As the owner decided (candidate 6), both now promise 64 KiB and state that the larger limit is not promised; the code is unchanged.
17. Resolved after `0.19.0`: H said server CLI overrides need an explicit `--url` or matching environment or configuration settings, while the code falls back to PID 1's server options when the first probe fails ([healthcheck URL selection](#healthcheck-url-selection)). H now describes the fallback.

## Candidates to change before the freeze

Decided items state the owner's decisions, the required changes and the open points; other items remain proposals. A freeze is expensive only for what cannot be fixed compatibly afterwards, so the candidates are split on that question, and each one states whether it can be. Within each group, the items most likely to stop a working setup or misstate behavior come first.

### Cannot be fixed compatibly after the freeze

Defaults, value meanings, reserved names and the definition of the contract. These need a decision before the first release candidate.

#### 1. Release candidates publish exact tags and remain supported upgrade sources

- **Reason:** Step 3 of the gate must publish and soak `1.0.0-rc.N`; the former stable-only validation prevented that publication.
- **Owner decisions:** A candidate never moves any floating tag, including `X.Y`, any major alias, and `latest`. Stable releases compute aliases from stable versions alone. Published recipes explicitly pin their own source release's exact candidate image; the README quick start retains stable `latest`. Every candidate's data directory is a supported upgrade source for step 2, with its own digest-pinned fixture that the final release must open.
- **Implemented scope:** The parsed xtask version and shared shell validator accept only stable versions or `-rc.N` with positive `N`, without leading zeros or build metadata. Binary/GHCR workflows and Docker Hub mirroring use that grammar; candidates retain exact versions in archives, checksums, metadata, digest reports and immutable tags, and GitHub marks them as prereleases without latest promotion. Upgrade-source ordering includes each candidate before the final release.
- **Verification limit:** Local policy, packaging-version and mocked publication checks pass; an actual candidate publication, its fixture capture from a published image and the 14-day soak remain unverified. Publication is a separately authorized action.

#### 2. Reserve the `GCS_MIMIC_*` prefix for server settings

- **Reason:** The server reads every `GCS_MIMIC_*` name as a setting and stops on an unknown one (`src/config_input.rs` `Inputs::environment_os`, `src/config.rs` `collect_settings`). The project also gives that prefix to names that are not server settings: client variables in the published examples (`GCS_MIMIC_ENDPOINT`, `GCS_MIMIC_GRPC_ENDPOINT`), the CLI's `GCS_MIMIC_SYNC_TOKEN`, the Go package's `GCS_MIMIC_BINARY`, and the Compose-only `GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE` and `GCS_MIMIC_TLS_DIR`. Its own tools work around the rule: `scripts/server-env.sh` and `childEnvironment` in `go/fakestorage/server.go` remove the prefix before starting a server.
- **Cost of keeping it:** An environment shared between an application and the emulator, such as one `.env` file, stops the server with `Unknown setting`; the examples make this the common case. `GCS_MIMIC_PORT` looks like the variable for `--port`, which has none. A future setting whose name matches a client variable, such as `endpoint`, would silently turn that variable into server configuration.
- **Breaking change if removed:** Renaming these variables breaks scripts, sample users and Compose overrides. Making unknown names non-fatal changes a documented rule (L, F) and lets a misspelled setting pass unnoticed.
- **Fixable compatibly after the freeze:** Partly. Relaxing the unknown-name rule, or ignoring specific names, is compatible. Moving names out of the prefix is not, and each new setting after the freeze can collide with a name users already set.
- **Owner decisions (2026-11-11):**
  - One rule: the server reads `GCS_MIMIC_*`, and every other tool reads `GCSM_*`, the prefix the repository's own `GCSM_TARGET_MAX_MB` already uses. An unknown `GCS_MIMIC_*` name stays fatal.
  - New names: `GCSM_ENDPOINT`, `GCSM_GRPC_ENDPOINT`, `GCSM_SYNC_TOKEN`, `GCSM_BINARY`, `GCSM_PORT`, `GCSM_IMAGE` and `GCSM_TLS_DIR`.
  - The old names are withdrawn in `0.18.0` without a transition period. When one is set, the server and the CLI stop and name its replacement; neither ignores it silently.
- **0.x default (2026-11-11, revisable before the freeze):** the 40 `GCS_MIMIC_*` names read only by tests, the SDK harnesses and repository scripts, listed below the [variable table](#environment-variables-outside-the-option-table), also move to `GCSM_*`, so that the rule stays one line.
- **Required change:** every item ships in `0.18.0`.
  - **Breaking:** the server's rejection. `rejected_setting_fix` in `src/config.rs` has dedicated advice for `port`, `image` and `tls_dir` today; it gets advice that names `GCSM_ENDPOINT`, `GCSM_GRPC_ENDPOINT`, `GCSM_SYNC_TOKEN`, `GCSM_BINARY`, `GCSM_PORT`, `GCSM_IMAGE` and `GCSM_TLS_DIR` for the old names. `serve` and `healthcheck` without `--url` stop on them already, because both validate the server environment; only the advice changes. `tests/it_process/foundation.rs` checks the current advice.
  - **Breaking:** `GCSM_SYNC_TOKEN` in `src/sync_cli.rs`: the reader, the `sync --help` text, and a new check that stops `gcs-mimic sync` with a pointer to `GCSM_SYNC_TOKEN` when `GCS_MIMIC_SYNC_TOKEN` is set. Tests in `tests/it_sync/sync_cli.rs` and `tests/it_sync/sync_cli/workspace.rs`; documents `docs/en/slices/sync-cli.md`, `docs/ja/sync-cli.md` and `docs/en/slices/directory-synchronization.md`.
  - **Breaking:** `GCSM_BINARY` in `go/fakestorage/server.go`: the `Executable` comment, the lookup, and an error that names `GCSM_BINARY` when `GCS_MIMIC_BINARY` is set. Without that error the old name would be ignored silently, because `childEnvironment` removes every `GCS_MIMIC_*` name from the child; that removal stays. Also `go/README.md`, `docs/en/slices/sdk-client-adapters.md`, and the tests and runners that set it: `go/fakestorage/runtime_test.go`, `go/fakestorage/fixture_integration_test.go`, `go/fakestorage/bucket_integration_test.go`, `sdk-tests/go/local_strict_test.go`, `sdk-tests/node/migration-adapters.mjs`, `sdk-tests/ruby/active_storage/run-strict.sh` and `scripts/sdk-go.sh`.
  - **Breaking:** `GCSM_ENDPOINT` and `GCSM_GRPC_ENDPOINT` in every example program and README under `examples/` (`examples/README.md`, and `main.go`, `main.mjs`, `main.py`, `main.rb`, `main.php`, `Main.java`, `Program.cs`, `src/main.rs` and `gcloud-example.sh` with each directory's `README.md`), in `docs/en/getting-started.md` and in `docs/en/slices/grpc-v2-foundation.md`.
  - **Breaking:** `GCSM_PORT`, `GCSM_IMAGE` and `GCSM_TLS_DIR` in `compose.yaml`, `compose.tls.yaml`, `examples/strict-local/compose.yaml` with the copy in its README that `scripts/check-docs.sh` compares byte for byte, `examples/full-stack/compose.yaml` and its README, `.github/workflows/ci.yaml` (`GCS_MIMIC_IMAGE`), and the documents that show them: `docs/en/getting-started.md`, `docs/en/native-binary.md`, `docs/en/local-https.md`, `docs/en/signed-urls.md`, `docs/en/delivery-plan.md`, `docs/en/container-usage.md`, `docs/en/fake-gcs-server-configuration.md`, `docs/en/fake-gcs-server-migration.md`, `docs/en/rails-active-storage.md`, `docs/en/slices/configuration-workflows.md`, `docs/en/slices/seed.md`, and the Japanese `container-usage.md`, `fake-gcs-server-configuration.md`, `fake-gcs-server-migration.md` and `rails-active-storage.md`.
  - The repository's runners and tools that set or read the seven names: `scripts/smoke-containers.sh`, `scripts/sdk-go.sh`, `scripts/sdk-active-storage.sh`, `scripts/strict-recipe-browser.sh`, `scripts/test-release-recipes.sh`, the comment in `scripts/server-env.sh`, the `check-client-containers` recipe in `Justfile`, `go/cmd/sdk-guard`, and the runners, harnesses and READMEs under `sdk-tests/`.
  - The 40 test and harness names. Code: `scripts/compare-reference-emulator.sh`, `scripts/sdk-go.sh`, `scripts/signed-browser.sh`, `scripts/test-check.sh`, `scripts/test-pre-push.sh`, `Justfile`, `xtask/src/differential.rs`, `xtask/src/differential/transport.rs`, `xtask/src/test_ids.rs`, `src/json/error.rs`, `src/storage/catalog_tests.rs`, `src/storage/catalog/import_journal/tests.rs`, `src/storage/catalog/sync_state/root/tests.rs`, `tests/it_admin/notifications_http.rs`, `go/cmd/sdk-guard/main.go`, `go/sourceexport/export_test.go`, and the Go, Node.js, PHP, Python, Ruby and Rust harnesses under `sdk-tests/`. Documents: `docs/en/development.md`, `docs/ja/development.md` (which explains why the `GCSM_TEST_` settings avoid the server prefix), `docs/en/getting-started.md` (`GCS_MIMIC_BIN`), `docs/ja/grpc-v2-foundation.md`, `go/sourceexport/README.md`, the `sdk-tests/` READMEs and `sdk-tests/go/verification.md`, and the slice documents under `docs/en/slices/` that give verification commands with these names. Ledger: the verification commands recorded in `compatibility/features.json` and `compatibility/gaps.json`.
  - Kept as recorded: the transcripts under `docs/en/evidence/` and past release notes show the commands that actually ran, so they keep the old names. The release notes of `0.18.0` list every rename.
  - The [environment variables outside the option table](#environment-variables-outside-the-option-table) and the [spelling rules](#spelling-rules) in this document, and `llms.txt`.
  - Tests: each old name stops `serve`, `healthcheck` without `--url` and, for the token, `gcs-mimic sync`, with advice that names the new name; a server started with every `GCSM_*` name set starts, which closes the gap listed under [contract areas without a test](#contract-areas-without-a-test-guarding-them); the Go package fails when `GCS_MIMIC_BINARY` is set.
- **Implemented scope (0.18.0):** every required change above. One enum, `RenamedEnvironmentVariable` in `src/config_input/environment.rs`, defines the seven names and the advice that the server and `gcs-mimic sync` share. The sync CLI rejects the old token name even when it is empty, a new token is set or `--token-file` is given. The Go package and the example programs stop on their old name only when no new name or explicit option supplies the value. The shipped Compose files pass `GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE` and `GCS_MIMIC_TLS_DIR` into the server only when set, so an old name left in a shell or `.env` file stops startup instead of selecting Compose's default silently; this answers the Compose limit below for the shipped files. The signed-browser script's binary variable merged into `GCSM_BINARY`, and the harness token variable became `GCSM_TOKEN`, resolving the first open point below. `scripts/check-docs.sh` derives the accepted server names from `Setting` and fails on any other `GCS_MIMIC_*` name outside the rejected inputs and the historical baseline.
- **Verification limit:** the process, sync, Go and documentation checks in the [verification record](evidence/environment-names.md) passed; container and SDK runs on the integrated source belong to the release verification.
- **Open points:**
  - Two moved test names sit close to decided names. `GCS_MIMIC_BIN`, the binary for `scripts/signed-browser.sh` that `docs/en/getting-started.md` shows, would become `GCSM_BIN` next to `GCSM_BINARY`; merging it into `GCSM_BINARY` avoids two names for one idea. `GCS_MIMIC_TOKEN` would become `GCSM_TOKEN` next to `GCSM_SYNC_TOKEN`. The other moved names collide with no existing `GCSM_*` name; the `GCS_MIMIC_TEST_*` names join the wrapper's `GCSM_TEST_JOBS`, `GCSM_TEST_RAMDISK` and `GCSM_TEST_RAMDISK_MB`.
  - Whether the example programs also stop when an old name is set. The decision names the server and the CLI; an example that still finds no `GCSM_ENDPOINT` falls back to its default address.
  - A limit, not a choice: Compose reads `GCSM_PORT`, `GCSM_IMAGE` and `GCSM_TLS_DIR` itself, and an old `GCS_MIMIC_PORT` left in a shell or `.env` file reaches the server only if the Compose file passes it into the container. Otherwise Compose uses its default for `GCSM_PORT` and `GCSM_IMAGE` without a message, and no gcs-mimic code can see the old name; only `compose.tls.yaml` stops, because it requires `GCSM_TLS_DIR`.

#### 3. Confirm the defaults and value meanings

Changing any of these changes behavior for everyone who left the setting unset, so each needs a major version after the freeze.

| Item | Current | Reason to reconsider | Cost of keeping it | Breaking change if changed |
| --- | --- | --- | --- | --- |
| `scheme` | `https` | `README.md` says its SDK examples use HTTP, and every setup in L sets `GCS_MIMIC_SCHEME: http` | Most users must opt out first | Clients that rely on HTTPS by default fail |
| `cors_fallback`, `signed_urls`, `test_signer`, `signed_host_port`, `local_creator_email` | The first four follow the authentication settings; the last takes effect only in native permissive mode | A default cannot be read from the setting alone | Every guide must state both modes | Any change alters configurations that do not set the value |
| `buckets_file_mode` | `create` for a file, `reconcile` inline (`BucketsFileMode::resolve`) | The same document behaves differently depending on how it is supplied | Each guide must explain the split | An edited document behaves differently at the next start |
| `data_dir` | `data`, relative to the working directory at run time | Starting from another directory opens a new empty catalog without notice; the image sets an absolute path | Native users can appear to lose data | Native setups that rely on `./data` must name it |
| `latest` image tag | Highest stable version, across major versions (`xtask/src/container.rs` `tags`) | `README.md` starts with it | A future major release can break quick-start users | Changing it alters what existing pulls receive |
| Boolean values | `admin_enabled` and `sync_delete_guard` take `true` or `false`; `test_signer` takes `enabled` or `disabled` | Two styles for the same idea | A minor inconsistency | Removing accepted values breaks settings |

- **Fixable compatibly after the freeze:** No, except that another boolean spelling can be added alongside the existing ones.
- **Owner decisions (2026-11-11):**
  - `scheme`: the default does not change in `0.18.0`; it stays `https`. It changes once, in the next minor release, together with a new capability: one port that accepts both HTTP and HTTPS connections. Returning the resumable-upload session URL from the address the client connected to is designed in the same work. The owner then decided that design's questions the same day: the new default is a new value, `auto`, under every profile; it uses the bundled development certificate unless both TLS paths are supplied, and the TLS paths alone do not make the listener HTTPS-only; `scheme=both` stays unchanged; `unix_socket` with `auto` serves plain HTTP; and fake-gcs-server's `--public-host` without `--scheme` still translates to `https://`. The [design](slices/single-port-scheme.md) records them with the migration steps. An earlier decision of the same day (default `http`, `compat_profile=legacy-emulator` keeps `https`, both TLS paths imply `https`) was withdrawn and is not implemented.
  - `data_dir`: the default `data` stays. Startup reports the data directory it opened and whether it created a new catalog or opened an existing one.
  - The other rows of the table (the settings that follow the authentication profile, `buckets_file_mode`, the `latest` image tag and the boolean spellings) are not decided and stay proposals for the freeze.
- **0.x default (2026-11-11, revisable before the freeze):** the data directory is reported as an absolute path, joined with the working directory but not canonicalized, in a ready-event field and in an `info` runtime event. With `backend=memory`, which never opens `data_dir`, both are omitted.
- **Required change for `data_dir`:** ships in `0.18.0`; see the implemented scope.
- **Implemented scope (0.18.0), `data_dir`:** `Catalog::open_for_server` returns the absolute path and a `DataDirState` of `created` or `opened`, decided under the directory lock from the stored schema version; `src/startup.rs` reports them as the ready fields `data_dir` and `data_dir_state` and emits the `info` event `data_dir_created` only when this start initialized the catalog. An existing directory without a catalog, or with an empty database, counts as `created`; once initialization commits, the next start reports `opened` even after a crash before the first ready event. Memory storage omits both fields and the event. Non-UTF-8 path bytes are shown with replacement characters. The process tests named `ready_event_*` in `tests/it_process/foundation.rs` pin each case; see the [verification record](evidence/ready-event.md) and the [startup contract](slices/startup-phases.md).
- **Verification limit:** the non-UTF-8 working-directory test runs only on Linux and has not run there yet; it belongs to the release verification.
- **Implemented in `0.19.0`, `scheme`:** [one port for HTTP and HTTPS](slices/single-port-scheme.md) is DESIGN-102, `IMPLEMENTED` since its client and signed-URL cases passed. It covers the default, the single port for HTTP and HTTPS, the session URL taken from the connection, and the healthcheck's URL selection, which now probes over HTTP unless `scheme=https` ([healthcheck URL selection](#healthcheck-url-selection)). The [0.19.0 release notes](releases/0.19.0.md) hold the migration steps. Nothing in `0.18.0` changes the scheme.
- **Open points for the freeze:**
  - Whether the `data_dir` default above stays.
  - The undecided rows of the table above.

#### 4. Make machine-readable names and exit codes contract, not wording

- **Reason:** The policy excluded diagnostic wording, but it did not say whether ready fields, diagnostic keys, event names or exit status are stable. On the reviewed source, `protocol` was always `http1` and `gcs_api` was a fixed string that predates XML and gRPC, though a test pinned both (`tests/it_process/foundation.rs`).
- **Cost of keeping it undecided:** Every field rename in 1.x becomes an argument, and stale values must stay forever if the ready event is contract.
- **Breaking change if removed:** Dropping or renaming a ready field breaks scripts that read it, such as the `jq` examples in N. Declaring the fields unstable breaks nothing but withdraws an implied promise.
- **Fixable compatibly after the freeze:** Partly. Promising more later is compatible; withdrawing a promise is not, and stale values can be dropped only before they are promised.
- **Owner decisions (2026-11-11):** Ready-event field names, diagnostic keys, event names, exit codes and the response fields of the user-facing emulator routes (candidate 6) are contract. Wording is not. The stale ready fields `protocol` and `gcs_api` are removed in a `0.x` release before the freeze.
- **Required change:** the removal ships in `0.18.0`; the documents and tests can follow in any release before the freeze.
  - **Breaking:** remove `protocol` and `gcs_api` from `ReadyEvent` in `src/startup.rs`, and from the expected field list and the value assertions in `tests/it_process/foundation.rs`. No document, example or SDK harness reads either field; the release notes name the removal. The ready-event row of [exit codes and machine-readable output](#exit-codes-and-machine-readable-output) drops them.
  - [Release policy](release-policy.md): states which machine-readable output is contract. That statement is true of the code today and does not wait for the removal.
  - Documents: an inventory of what is now contract. `docs/en/native-binary.md` names individual ready fields in examples and option rows, and only this document lists them all. Diagnostic sources are documented only partly (F), and runtime event names exist only as string literals at each `diagnostics::emit` call site in `src/`, with no list in code or documents.
  - Tests for what [has no guard](#contract-areas-without-a-test-guarding-them): the exit status value 1, the ready field sets of the other transport and authentication combinations, and a list of event names that fails when one is renamed or removed.
- **Implemented scope (0.18.0):** `ReadyEvent` no longer has `protocol` or `gcs_api`, and the release policy states which machine-readable output is contract. `process_shutdown_reports_stopped_and_releases_catalog_for_restart` in `tests/it_process/foundation.rs` pins the complete disk ready field set without the two fields, and `ready_event_omits_data_directory_and_creation_diagnostic_for_memory` checks their absence in memory mode. The document inventory and the guard tests listed above remain for a release before the freeze.
- **Open points for the freeze (outside `0.18.0`):**
  - Whether "diagnostic keys" covers only the member names of a diagnostics line (`event`, `level`, `diagnostics`, `source`, `key`, `fix`) or also the values of `source` and `key`. The `key` value is the input name as supplied, such as `GCS_MIMIC_LOG_LEVEL`, `-cors-headers` or `data_dir`.
  - Whether the fields of runtime events other than `event` and `level` are contract, and whether the import output (`import_page`, `import_complete` and their fields) counts as events.
  - Whether ready-field values are contract as well as names, for example the strings that `transport`, `backend` and `auth_mode` report.
  - The `gcs-mimic sync` output. The command writes the sync route's JSON response to stdout unchanged (`src/sync_cli.rs`), and candidate 6 places the sync routes outside the contract, so the output fields of a documented command are not covered by either decision.

#### 5. Accept a fake-gcs-server flag only when it translates exactly

- **Reason:** On the reviewed source, seventeen migration flags would have been frozen with their translations, including `filesystem-root`, which was accepted and ignored.
- **Cost of keeping them:** Each flag and its value handling must survive every 1.x release, and each one adds to the CLI parser's special cases.
- **Breaking change if removed:** Removing a flag stops command lines copied from fake-gcs-server. A policy line that makes aliases deprecatable with notice breaks nothing now.
- **Fixable compatibly after the freeze:** No. Once the flags are inside the contract, removing one or excluding them later withdraws a promise.
- **Owner decisions (2026-11-11, final classification):**
  - `--host`, `--port` and `--port-http` are native options and part of the stable contract, like `--scheme` and `--log-level`.
  - The other flags in the [alias table](#alias-flags) come from fake-gcs-server and follow one rule: a flag is accepted when every request that worked because of it under fake-gcs-server gives the same result after translation; a translation that also serves more requests is fine. Otherwise the flag is refused.
  - Accepted and translated, with startup showing the native form: `public-host`, `data`, `backend` (`filesystem`), `cert-location`, `private-key-location`, and the `event.*` flags when a Pub/Sub emulator host is configured.
  - Refused with a diagnostic that names the native replacement: `filesystem-root`, `cors-headers` in every authentication mode (candidate 7), `location`, `external-url`, and the `event.*` flags without an emulator host. fake-gcs-server can publish to Google, which gcs-mimic never does.
  - Known difference recorded for `public-host`: requests to a subdomain of the public host are treated as virtual-hosted buckets, so a JSON API client must use a host outside those subdomains or set `api_base_url`.
  - Accepted fake-gcs-server flags are outside the stable contract. The deprecation notice period is decided at the freeze.
- **Implemented scope (0.18.0):** `src/config_input.rs` separates `NativeFlag`, `AliasFlag` and `RefusedFlag`. Native flags print no notice. Accepted flags print one `info` diagnostic with key `native_equivalent` whose `native_args` array holds the complete native options, and, when no value needs escaping, a shell line to copy; `log_level=warn` or `error` hides it. Every refused flag gives an `error` diagnostic keyed by the flag as supplied, all in one batch, with exit status 1, no ready event and no catalog. The code is stricter than the list above in two places: `event.list none` is refused even with an emulator host, because the source has no such selection (native `--event-list none` remains), and `--port-http` without `scheme=both` now stops startup instead of being ignored with a warning. Helper commands keep rejecting the fake-gcs-server flags with advice. S `option-aliases.md` lists the acceptance tests and the reason for each refusal.
- **Verification limit:** the targeted Rust suites listed in S `option-aliases.md` passed; the full suite, container builds and SDK harnesses on the integrated source belong to the release verification.
- **Open points for the freeze:**
  - The deprecation notice period for accepted fake-gcs-server flags, and where the notice appears.
  - Whether `--host`, `--port` and `--port-http` keep their alias grammar. As native options they are still command-line only, take one or two hyphens and have no underscore form; the environment and JSON set `listen` and `listen_http` instead.
  - Whether `backend=filesystem` from the environment and JSON (candidate 14) follows the same rule as the flag.

#### 6. Make the user-facing `/_internal/*` routes contract, not the sync routes

- **Reason:** The policy's list does not name these routes. Their body keys mix styles: `/_internal/config` uses camelCase (`externalUrl`, `publicHost`, the reference emulator's names), and the sync routes use snake_case (`deletion_set`, `timeout_ms`, `conflict_id`). The documents give a 64 KiB body limit for management routes, while `/_internal/config` and `/_internal/reset` read up to 16 MiB and the sync routes read up to 64 KiB.
- **Cost of keeping it undecided:** Tools written against either limit or key style may break without a clear rule.
- **Breaking change if removed:** Renaming keys breaks tools, including older `gcs-mimic sync` clients. Lowering a limit rejects bodies that work today.
- **Fixable compatibly after the freeze:** Partly. Accepting another key spelling or raising a limit is compatible; removing a spelling or lowering a limit is not.
- **Owner decisions (2026-11-11):** The user-facing routes (health check, ready, config, reset and lifecycle run) are contract. Their keys are frozen as currently spelled, and 64 KiB is the guaranteed request body size; larger bodies that the implementation accepts today are not promised. The sync routes are the internal protocol between a server and `gcs-mimic sync` of the same version and are outside the contract.
- **Keys frozen by the decision, as spelled on the checked source:**

  | Route | Request | Response |
  | --- | --- | --- |
  | `/_internal/healthcheck`, `/_internal/ready` | No body | `status`, `unix_time`; `{"status":"not ready"}` with 503 before readiness (`probe` in `src/server.rs`) |
  | `/_internal/config` | `externalUrl`, `publicHost` | 200 with an empty body (`update` in `src/server/admin.rs`) |
  | `/_internal/reset` | `confirm` | `buckets`, `objects`, `cleanupPending` (`ResetOutcome` in `src/storage/catalog/admin/reset.rs`) |
  | `/_internal/lifecycle/run` | Empty body | `aborted`, `enabled`, `scanned`, `deleted`, `archived`, `transitioned`, `blocked`, `skipped` (`LifecycleResponse` in `src/storage/catalog/lifecycle.rs`) |

  The frozen keys therefore keep both styles: `unix_time` is snake_case, and `externalUrl`, `publicHost` and `cleanupPending` are camelCase.
- **Required change:** None in code; the decision keeps every key and the current limits.
  - [Release policy](release-policy.md): names the contract routes and the sync exclusion. That statement is true today.
  - Documents: `docs/en/slices/runtime-admin-endpoints.md` and `docs/en/slices/local-instance-reset.md` call 64 KiB a bound or cap; they must call it the guaranteed size, without promising that larger bodies are refused or accepted ([disagreement 16](#disagreements-between-code-and-documents)). `docs/en/slices/sync-cli.md`, `docs/ja/sync-cli.md` and `docs/en/slices/external-deletion-controls.md` say the sync routes are an internal protocol tied to the server's version, and direct users to `gcs-mimic sync`.
  - Tests: a 64 KiB body is accepted by `/_internal/config` and `/_internal/reset` (for example a valid body padded with whitespace), and the response keys of each contract route are pinned. Routes that take no body keep rejecting any body: the probes answer 413 and lifecycle run answers 400, so the 64 KiB guarantee applies only where a body is accepted.
- **Open points for the freeze (outside `0.18.0`):**
  - Routes the decision does not name: `/_internal/health` (liveness, the same response as the probes), `/_internal/objects` (fixture upload), `/_internal/delete_all` (the reference emulator's reset form, served by the reset handler) and `/_internal/reseed`. Each is documented as user-facing.
  - Whether the error responses are contract. The probes answer `{"error":"..."}` with a string, and the management routes answer the JSON API error object (`error.code`, `error.message`, `error.errors[].reason`) from `JsonError` in `src/json/error.rs`.
  - Whether the methods, status codes and the 403 and 421 answers of `src/server/admin/guard.rs` are part of the route contract or only the keys.
  - How "the same version" is enforced for the sync routes. Nothing in the code checks that `gcs-mimic sync` and the server are the same release, so an older client reaches a newer server unchecked. A version check would be a code change.
  - The `gcs-mimic sync` output fields, raised under candidate 4.

#### 7. Stop on `--cors-headers` in every mode

- **Reason:** In strict mode the flag sets `cors_fallback=allow-all`, and its warning says that strict mode follows each bucket's rules. The warning appears, but its content is the opposite of the effect. Because the flag comes from the command line, it also beats `GCS_MIMIC_CORS_FALLBACK=none` in the environment.
- **Cost of keeping it:** Buckets without `cors` rules allow every origin in strict mode, for example when a `command` carried over from fake-gcs-server keeps the flag.
- **Breaking change if removed:** Making it a no-op in strict mode changes behavior for anyone relying on it. Changing only the warning breaks nothing.
- **Fixable compatibly after the freeze:** The warning, yes, because wording is not contract. The effect, no.
- **Owner decisions (2026-11-11):** Under the rule of candidate 5, `--cors-headers` cannot translate exactly: it discards its header list and selects `allow-all`. It therefore stops startup in every authentication mode. The error shows the native spelling: set `cors` on the buckets that need rules, or set `cors_fallback=allow-all` where every origin must be allowed.
- **Implemented scope (0.18.0):** `cors-headers` is a `RefusedFlag`. It stops startup in permissive and strict mode alike, so it can no longer override an explicit `GCS_MIMIC_CORS_FALLBACK=none`. The error names bucket `cors` rules and an explicit `cors_fallback=allow-all`. `refused_flags_report_every_original_key_before_startup` covers both authentication modes; see S `option-aliases.md`.
- **Open points:** none beyond those of candidate 5.

### Can be fixed compatibly after the freeze

Renames and grammar. These can keep the old name as an alias through 1.x, or add a form, so they do not need to happen before the freeze.

#### 8. Rename `buckets_file_mode`

- **Reason:** The setting also governs the inline `buckets` document. The ready field `buckets_file` has the same misleading name. Candidate 4 makes ready field names contract, so after the freeze the field can be renamed only by reporting both names.
- **Cost of keeping it:** A misleading name in every guide.
- **Breaking change if removed:** Removing the old flag, variable and JSON key breaks configurations.
- **Fixable compatibly after the freeze:** Yes, with the old name kept as an alias.

#### 9. Reduce duplicate spellings in `gcs-mimic sync`

- **Reason:** `sync findings` and `sync status --view findings` do the same thing, as do `--deletion-set` and `--revision` on `approve`, and `--timeout` and `--timeout-ms`.
- **Cost of keeping it:** Three pairs to document, test and keep consistent.
- **Breaking change if removed:** Scripts using the removed spelling fail with a usage error.
- **Fixable compatibly after the freeze:** Yes, by naming one spelling as primary and keeping the other until a major version.

#### 10. Rename `connection_timeout_ms`

- **Reason:** It sets the header, body and request timeouts and nothing about connection lifetime or `keep_alive_timeout_ms`, so its name misleads.
- **Cost of keeping it:** A coarse alias with its own precedence rule stays in the settings code.
- **Breaking change if removed:** Users must set the three timeouts by name.
- **Fixable compatibly after the freeze:** Yes, with the old name kept as an alias.

#### 11. Choose names that do not expose implementation

- **Reason:** `catalog_wait_timeout_ms` and `memory_catalog_max_bytes` name the SQLite catalog, and `worker_threads` and `max_blocking_threads` name the Tokio runtime.
- **Cost of keeping it:** If the runtime or catalog changes in 1.x, these must stay as approximations or no-ops.
- **Breaking change if removed:** Removing the old flag, variable and JSON key breaks configurations.
- **Fixable compatibly after the freeze:** Yes, with the old names kept as aliases.

#### 12. Reconsider the name `legacy-emulator`

- **Reason:** The value describes fake-gcs-server behavior only indirectly, and a second behavior profile would be carried through 1.x.
- **Cost of keeping it:** An unclear name, and each new behavior must consider both profiles.
- **Breaking change if removed:** Removing the value breaks configurations that select it. Removing the profile breaks the migrations that rely on it.
- **Fixable compatibly after the freeze:** The name, yes, with the old value kept as an alias. Removing the profile, no.

#### 13. Decide whether `--foo_bar` spellings are supported

- **Reason:** Underscore spellings work by accident of normalization, are undocumented, and do not apply to alias flags.
- **Cost of keeping it:** Each option has at least four accepted spellings if they are later documented or relied on.
- **Breaking change if removed:** Rejecting them breaks only undocumented use.
- **Fixable compatibly after the freeze:** Yes while they stay undocumented, because the policy covers documented options. The owner should say so explicitly.

#### 14. Limit `backend=filesystem` to the flag, or document it

- **Reason:** The value exists for fake-gcs-server flags but is also accepted from the environment and JSON, against the documented value list and the error text.
- **Cost of keeping it:** A third value for one setting, undocumented outside the alias table.
- **Breaking change if removed:** Rejecting it from the environment and JSON breaks only undocumented use.
- **Fixable compatibly after the freeze:** Yes while it stays undocumented.

#### 15. Add a major image alias

- **Reason:** There is no `1` tag, so users cannot follow a major line. The meaning of `latest` is part of candidate 3.
- **Cost of keeping it:** Users choose between an exact or minor pin and `latest`.
- **Breaking change if removed:** None; a new tag is additive.
- **Fixable compatibly after the freeze:** Yes.

#### 16. Add a format version where none exists

- **Reason:** The bucket document requires `version: 1`, but the server JSON and identity registry have none.
- **Cost of keeping it:** A later incompatible format has no marker to select it.
- **Breaking change if removed:** None for an optional version key.
- **Fixable compatibly after the freeze:** Yes.

#### 17. Unify the helper-command grammar

- **Reason:** Server options accept one or two hyphens and `=`. Helpers accept only `--name VALUE`, with three separate parsers, and `--help` works only in some positions.
- **Cost of keeping it:** Users learn two grammars.
- **Breaking change if removed:** None, because accepting more forms is additive.
- **Fixable compatibly after the freeze:** Yes.

## Contract areas without a test guarding them

- Helper help texts. `URL_PUBLIC_HELP`, `URL_SIGN_HELP`, `TEST_SIGNER_HELP` and the `sync` and `import` help have no digest.
- Underscore spellings such as `--foo_bar`.
- `backend=filesystem` from the environment or JSON.
- The ready event's optional field combinations. `process_shutdown_reports_stopped_and_releases_catalog_for_restart` pins the complete disk field set over HTTP, including `data_dir`, `data_dir_state` and effective `lifecycle_execution`, and the memory case pins the omitted directory fields; other transport/authentication combinations remain separate checks.
- The exit status value. Tests check `success()` or its negation, not the value 1.
- The healthcheck's fallback to PID 1's server options. `container_server_args` has no test, and the only probe without `--url` in `scripts/smoke-containers.sh` runs against a container with no server options.
- Image metadata other than the user and data paths. `scripts/smoke-containers.sh` does not check `EXPOSE`, the `HEALTHCHECK` parameters, `SQLITE_TMPDIR` or the licenses path.
- `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS` with an empty value, which `settings_that_define_an_empty_environment_value_start_with_it` omits.

Since `0.18.0`, a server started with every `GCSM_*` name set is tested, and so is each withdrawn `GCS_MIMIC_*` client name ([verification](evidence/environment-names.md)); that area is no longer unguarded.

The option list is no longer unguarded either. `--help` is built from one exhaustive `match` with a line for every `Setting`, so a new setting does not compile without one, and lists the settings in their declaration order after the common ones. The tests in `src/config/help.rs` require it to list every `Setting` except `config` plus the CLI-only `--port`, `--port-http` and `--host` once, the option table of `docs/en/native-binary.md` to name each of them and `config` once with its `GCS_MIMIC_*` variable, and the "Every server option" list of `llms.txt` to name every `Setting` once. `scripts/check-docs.sh` runs them, so a documentation-only change is checked too. The table had lacked 18 options and `llms.txt` lacked `--lifecycle-execution`; both now list them.

## Open at the freeze, not part of 0.18.0

These points are recorded under their candidates and are decided at the freeze:

- The deprecation notice period for the accepted fake-gcs-server flags, and where the notice appears (candidate 5).
- The `/_internal/*` routes that the decision of candidate 6 does not name: `/_internal/health`, `/_internal/objects`, `/_internal/delete_all` and `/_internal/reseed`.
- Whether "diagnostic keys" covers only the member names of a diagnostics line or also the values of `source` and `key` (candidate 4).
- The standard output of `gcs-mimic sync`, which neither candidate 4 nor candidate 6 covers.

The `scheme` default is not a freeze point: it is decided and changes in the next minor release (candidate 3).
