# Public contract review for the 1.0.0 freeze

This document supports step 1 of the [1.0.0 gate](release-policy.md#the-100-gate), "Freeze the public contract". It inventories the stable surface and proposes changes for the project owner to decide. It changes nothing and decides nothing.

Reviewed source: version `0.17.1`, commit `a53a1f8c`, on 2026-11-10.

Lifecycle Delete execution uses the `lifecycle_execution` option below. It defaults to disabled with directory synchronization and enabled otherwise; an explicit choice overrides that default.

## How to read this document

Every claim about behavior comes from reading the code. The behaviors marked "observed" were also run against a debug build of the reviewed source. The supported API subset is not repeated here, because [`compatibility/features.json`](../../compatibility/features.json) is its only source.

Document abbreviations used in the tables:

| Abbreviation | Document |
| --- | --- |
| H | `--help` text, the `HELP` constant in `src/main.rs` |
| L | `llms.txt` |
| N | `docs/en/native-binary.md` |
| C | `docs/en/container-usage.md` |
| F | `docs/en/fake-gcs-server-configuration.md` |
| R | `README.md` |
| S | The named file under `docs/en/slices/` |

The "Agree" column says whether code and documents agree. A number refers to the [list of disagreements](#disagreements-between-code-and-documents).

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

Any `GCS_MIMIC_*` name that is not a setting stops `serve` and makes `healthcheck` without `--url` fail. This includes `GCS_MIMIC_SYNC_TOKEN` (observed) and the client variables that the published examples use; see [environment variables outside the option table](#environment-variables-outside-the-option-table).

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

These flags stand for native settings, mainly for fake-gcs-server command lines. All are defined by `AliasFlag` and `translation` in `src/config_input.rs` and listed in S `option-aliases.md` and F. They take one or two hyphens and no underscore form. `healthcheck`, `url`, `tls` and `test-signer` reject them with advice; `sync` and `import` reject them as unknown options.

| Flag | Becomes | Startup notice | Agree |
| --- | --- | --- | --- |
| `host` | IP of `listen`; `localhost` becomes `127.0.0.1` | None for a valid value | Yes |
| `port` | Port of `listen` | None for a valid value | Yes |
| `port-http` | Port of `listen_http` | Ignored with a warning unless `scheme=both` | Yes |
| `external-url` | `api_base_url` | None for a safe URL | Yes |
| `public-host` | `public_base_url` with the scheme of `scheme` | Warns | 6 |
| `data` | `seed_dir` | None | Yes |
| `backend filesystem` | `backend=disk` | None | 4 |
| `filesystem-root` | Nothing; ignored | Warns | Yes |
| `cert-location` | `tls_cert` | None | Yes |
| `private-key-location` | `tls_key` | None | Yes |
| `cors-headers` | `cors_fallback=allow-all`; the value is discarded | Warns | 5 |
| `location` | `default_location` | None for a supported location | Yes |
| `event.pubsub-project-id` | `event_pubsub_project_id` | Warns | Yes |
| `event.pubsub-topic` | `event_pubsub_topic` | Warns | Yes |
| `event.bucket` | `event_bucket` | Warns | Yes |
| `event.object-prefix` | `event_object_prefix` | Warns | Yes |
| `event.list` | `event_list` | Warns | Yes |
| `scheme`, `log-level` | Native options on `serve`; listed so helper commands can name them | None | Yes |

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
2. It probes `/_internal/healthcheck` at the configured listener. A wildcard address becomes loopback, and `scheme=both` selects the HTTP listener.
3. If that probe fails, it reads `/proc/1/cmdline`. When PID 1 is `gcs-mimic` running server options, it validates those options, derives a second URL from them and probes it.

An explicit `--url` skips all three steps. Step 3 is what lets the image's `HEALTHCHECK` pass when a Compose `command` such as `--scheme http` configures the server.

### Environment variables outside the option table

| Variable | Effect | Defined in | Documented in | Agree |
| --- | --- | --- | --- | --- |
| `PUBSUB_EMULATOR_HOST` | Same as `GCS_MIMIC_PUBSUB_EMULATOR_HOST`, which wins when both are set | `src/config_input.rs`, `src/config.rs` | H, L, F | Yes |
| `GCS_MIMIC_SYNC_TOKEN` | Bearer token for `gcs-mimic sync`; `--token-file` wins. Stops `serve` and `healthcheck` without `--url` if set for them | `src/sync_cli.rs` | `sync --help`, S `sync-cli.md` | Yes |
| `GCS_MIMIC_ENDPOINT` | Address for the client programs in `examples/`. The server does not read it; it stops `serve` and `healthcheck` without `--url` if set for them | `examples/*`, for example `examples/go/main.go` | `examples/README.md` and each example's README, `docs/en/getting-started.md` | Yes |
| `GCS_MIMIC_GRPC_ENDPOINT` | gRPC address for `examples/rust`. Same effect on the server | `examples/rust/src/main.rs` | `examples/rust/README.md` | Yes |
| `GCS_MIMIC_BINARY` | Executable that the Go package starts. Same effect on the server, although the package removes every `GCS_MIMIC_*` name from the child it starts | `go/fakestorage/server.go` | `go/README.md` | Yes |
| `GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE`, `GCS_MIMIC_TLS_DIR` | Compose interpolation in `compose*.yaml` and `examples/`; the server rejects each with advice | `src/config.rs` `rejected_setting_fix` | N, F | Yes |
| `SQLITE_TMPDIR` | Set by the image; read by SQLite, not by gcs-mimic | `Dockerfile` | C | Yes |

`FAKE_GCS_*` variables are not read (`src/config_input.rs` test `fake_gcs_server_variables_are_not_read`; L, F). `STORAGE_EMULATOR_HOST` and similar variables belong to clients.

Every `GCS_MIMIC_[A-Z0-9_]+` name in the repository was compared with `Setting`: 117 distinct names, of which 70 are settings. Besides the 7 names in the table above, 4 are examples or test inputs for unknown settings (`FOO_BAR`, `UPPER_SNAKE_CASE`, `NO_SUCH_SETTING`, `UNKNOWN_SETTING`). The other 40 are read only by tests, the SDK harnesses and repository scripts, and are not part of the contract: `ALLOWED_HOSTS`, `BIN`, `BUCKET`, `CA_CERT`, `CHECK_FAIL`, `CHECK_LOG`, `CHECK_NO_TESTS`, `CHECK_OVERLAP`, `CHECK_ROOT`, `DIFF_ENDPOINT`, `DIFF_GRPC_BEARER`, `DIFF_PROJECT`, `DIFF_TOKEN`, `DOCKER_GATEWAY`, `EGRESS_PROXY`, `GO_TEST_RUN`, `IMPORT_CRASH_CATALOG`, `IMPORT_CRASH_COMMITTED`, `LOCAL_STRICT_ENDPOINT`, `PHP_DOCKER_NETWORK`, `PRE_PUSH_TEST_FAIL`, `PRE_PUSH_TEST_SERIAL`, `PRE_PUSH_TEST_SLEEP`, `PRE_PUSH_TEST_STATE`, `REFERENCE_ONLY`, `REFERENCE_REPO`, `ROOT_LOCK_TEST`, `SDK_SCHEME`, `SIGNED_TESTS`, `SOURCE_FIXTURE_DIR`, `STRICT_ENDPOINT`, `STRICT_GRPC_ENDPOINT`, `TEST_BACKEND_LOG`, `TEST_LOCK_DIR`, `TEST_PUBSUB_EMULATOR_HOST`, `TEST_RECOVERY_DIR`, `TEST_REJECTED_WAL_DIR`, `TEST_REJECTED_WAL_FIXTURE`, `TOKEN` and `TRUSTED_ENDPOINT`.

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
| Schema | Version 52; older catalogs migrate forward on open; downgrade is not supported | `src/storage/catalog.rs` `CATALOG_SCHEMA_VERSION` | N, `docs/en/release-policy.md` | Yes |
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
| Ready event | One JSON line on stdout with `event: "ready"`, `executable`, `version`, `healthcheck_path`, `admin_enabled`, effective `lifecycle_execution` (`enabled` or `disabled`), `message`, `listen`, `listeners`, optional `trusted_listen` and `trusted_principal`, `transport`, optional `unix_socket`, `protocol`, `gcs_api`, the limits and timeouts, `backend`, `auth_mode`, optional `identities`, `test_signer`, `signed_urls`, `seed`, `buckets_file`, `scheme`, `public_base_url`, `public_virtual_host_enabled`, `cors_fallback`, `signed_host_port`, optional `signed_host_port_signers`, `api_base_url` | `src/startup.rs` `ReadyEvent` | N, C, S `runtime-admin-endpoints.md`, S `lifecycle-execution.md` | Yes |
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

1. `--sync-delete-guard` help says "deletion ingestion pending". The code applies the guard during deletion ingestion (`src/sync/deletion.rs`), and C describes it as shipped. The help text is stale.
2. `--seed-dir` help says "image: /data". The image sets `GCS_MIMIC_DEFAULT_SEED_DIR`, not `seed_dir` (`Dockerfile`).
3. `--default-location` help says `US|EU|ASIA`. The code also accepts every region and dual-region in `src/bucket.rs`, as F says.
4. `backend` accepts `filesystem` from the environment and JSON as well as the flag (`src/config/storage.rs`; observed with `GCS_MIMIC_BACKEND=filesystem`). H, N and the error text say `disk` or `memory`. S `option-aliases.md` presents it as a flag translation only, and the helper advice in `alias_advice` says `filesystem` or `memory`.
5. The `--cors-headers` warning says "strict mode follows each bucket's cors rules". The flag sets `cors_fallback=allow-all` in strict mode too (observed: ready reports `allow-all`), which contradicts the warning and the comment in `alias_advice`.
6. S `option-aliases.md` says `public-host` warns that Host matching needs the legacy profile. The `serve` warning (`alias_difference`) does not mention the profile; only the helper-command advice (`alias_advice`) does.
7. N says `buckets` and `identities` have the "same limits" as their file forms. In a JSON config file they are bound by that file's 64 KiB limit (`read_config`).
8. `--event-list` help says `none` disables the global rule. Alone it fails startup, because event filters need a project and topic (`src/config/notifications.rs`; observed).
9. C's list of settings that define an empty value omits `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`, which the code accepts empty when `trusted_listen` is set (`src/config.rs` `allowed_hosts`).
10. L lists the bucket entry's optional fields as `cors`, `iamPolicy`, `versioning` and `softDeletePolicy`. The code also accepts `project`, `location`, `storageClass`, `labels`, `defaultEventBasedHold` and `iamConfiguration` (`src/bucket_init.rs` `Entry`).
11. The main help lists `sync findings` and `sync deletions`, and the code accepts them, but `sync --help` shows only `status --view`. `sync approve` also accepts `--revision`, which its help omits; its error message names `--revision` while the help names `--deletion-set`.
12. `url sign --help` lists GET, HEAD, PUT and DELETE. The code also signs POST when `x-goog-resumable: start` is supplied (`src/auth/signature.rs`).
13. The main help's usage line omits `--h` and `-version`, which the code accepts.
14. The release policy requires publishing `1.0.0-rc.N`. The release tooling refuses prerelease versions in `xtask/src/container.rs` `tags`, in the version checks of `.github/workflows/binaries.yaml` and `.github/workflows/ghcr.yaml`, and in `scripts/container-mirror.sh` through `container-tags`. A test in `xtask/src/container.rs` and `scripts/test-container-mirror.sh` pin the refusal. Step 3 of the gate cannot run until this changes; see candidate 1.
15. `--memory-max-bytes` help and L say half of available memory. The code subtracts 32 MiB first, and it uses 1 GiB wherever it cannot read available memory, which is everywhere except Linux (`src/memory_limit.rs`).
16. S `runtime-admin-endpoints.md` and S `local-instance-reset.md` say management bodies are capped at 64 KiB. The handlers read at most `RESOURCE_BODY_LIMIT`, which is 16 MiB (`src/json/body.rs`, `src/server/admin.rs`, `src/server/admin/reset.rs`).
17. H says server CLI overrides need an explicit `--url` or matching environment or configuration settings. The code falls back to PID 1's server options when the first probe fails ([healthcheck URL selection](#healthcheck-url-selection)), as C says.

## Candidates to change before the freeze

These are proposals for the project owner; nothing here is decided. A freeze is expensive only for what cannot be fixed compatibly afterwards, so the candidates are split on that question, and each one states whether it can be. Within each group, the items most likely to stop a working setup or misstate behavior come first.

### Cannot be fixed compatibly after the freeze

Defaults, value meanings, reserved names and the definition of the contract. These need a decision before the first release candidate.

#### 1. Make release candidates publishable and decide what they mean

- **Reason:** Step 3 of the gate publishes `1.0.0-rc.N`, and every release path refuses it: `xtask/src/container.rs` `tags`, the version checks in `.github/workflows/binaries.yaml` and `.github/workflows/ghcr.yaml`, and `scripts/container-mirror.sh` through `container-tags`. A test in `xtask/src/container.rs` and `scripts/test-container-mirror.sh` pin the refusal. `scripts/container-release.sh` and `scripts/container-mirror.sh` also derive the minor alias with `${version%.*}`, which turns `1.0.0-rc.1` into `1.0.0-rc`.
- **Decisions needed:** Whether a candidate moves `X.Y` and `latest`. Whether a data directory written by a candidate counts as an upgrade source for step 2 of the gate.
- **Cost of keeping it:** The gate cannot pass.
- **Breaking change if removed:** None to accept prerelease versions. The two decisions above are hard to reverse once a candidate is published.
- **Fixable compatibly after the freeze:** Not applicable; step 3 cannot run until it is done.

#### 2. Decide the policy for the reserved `GCS_MIMIC_*` prefix

- **Reason:** The server reads every `GCS_MIMIC_*` name as a setting and stops on an unknown one (`src/config_input.rs` `Inputs::environment_os`, `src/config.rs` `collect_settings`). The project also gives that prefix to names that are not server settings: client variables in the published examples (`GCS_MIMIC_ENDPOINT`, `GCS_MIMIC_GRPC_ENDPOINT`), the CLI's `GCS_MIMIC_SYNC_TOKEN`, the Go package's `GCS_MIMIC_BINARY`, and the Compose-only `GCS_MIMIC_PORT`, `GCS_MIMIC_IMAGE` and `GCS_MIMIC_TLS_DIR`. Its own tools work around the rule: `scripts/server-env.sh` and `childEnvironment` in `go/fakestorage/server.go` remove the prefix before starting a server.
- **Decisions needed:** Where names that are not server settings belong. Whether an unknown name stays fatal.
- **Cost of keeping it:** An environment shared between an application and the emulator, such as one `.env` file, stops the server with `Unknown setting`; the examples make this the common case. `GCS_MIMIC_PORT` looks like the variable for `--port`, which has none. A future setting whose name matches a client variable, such as `endpoint`, would silently turn that variable into server configuration.
- **Breaking change if removed:** Renaming these variables breaks scripts, sample users and Compose overrides. Making unknown names non-fatal changes a documented rule (L, F) and lets a misspelled setting pass unnoticed.
- **Fixable compatibly after the freeze:** Partly. Relaxing the unknown-name rule, or ignoring specific names, is compatible. Moving names out of the prefix is not, and each new setting after the freeze can collide with a name users already set.

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

#### 4. Decide which machine-readable output is contract

- **Reason:** The policy excludes diagnostic wording, but it does not say whether ready fields, diagnostic keys, event names or exit status are stable. `protocol` is always `http1` and `gcs_api` is a fixed string that predates XML and gRPC, though a test pins both (`tests/it_process/foundation.rs`).
- **Cost of keeping it undecided:** Every field rename in 1.x becomes an argument, and stale values must stay forever if the ready event is contract.
- **Breaking change if removed:** Dropping or renaming a ready field breaks scripts that read it, such as the `jq` examples in N. Declaring the fields unstable breaks nothing but withdraws an implied promise.
- **Fixable compatibly after the freeze:** Partly. Promising more later is compatible; withdrawing a promise is not, and stale values can be dropped only before they are promised.

#### 5. State whether the fake-gcs-server aliases are inside the 1.x contract

- **Reason:** Seventeen migration flags would be frozen with their exact translations, including `filesystem-root`, which is accepted and ignored.
- **Cost of keeping them:** Each flag and its value handling must survive every 1.x release, and each one adds to the CLI parser's special cases.
- **Breaking change if removed:** Removing a flag stops command lines copied from fake-gcs-server. A policy line that makes aliases deprecatable with notice breaks nothing now.
- **Fixable compatibly after the freeze:** No. Once the flags are inside the contract, removing one or excluding them later withdraws a promise.

#### 6. Decide what the `/_internal/*` contract is

- **Reason:** The policy's list does not name these routes. Their body keys mix styles: `/_internal/config` uses camelCase (`externalUrl`, `publicHost`, the reference emulator's names), and the sync routes use snake_case (`deletion_set`, `timeout_ms`, `conflict_id`). The documents give a 64 KiB body limit for management routes, while `/_internal/config` and `/_internal/reset` read up to 16 MiB and the sync routes read up to 64 KiB.
- **Decisions needed:** Whether the routes are contract at all. Which key style is kept. Which body limit is the promise.
- **Cost of keeping it undecided:** Tools written against either limit or key style may break without a clear rule.
- **Breaking change if removed:** Renaming keys breaks tools, including older `gcs-mimic sync` clients. Lowering a limit rejects bodies that work today.
- **Fixable compatibly after the freeze:** Partly. Accepting another key spelling or raising a limit is compatible; removing a spelling or lowering a limit is not.

#### 7. Correct what `--cors-headers` does in strict mode

- **Reason:** In strict mode the flag sets `cors_fallback=allow-all`, and its warning says that strict mode follows each bucket's rules. The warning appears, but its content is the opposite of the effect. Because the flag comes from the command line, it also beats `GCS_MIMIC_CORS_FALLBACK=none` in the environment.
- **Cost of keeping it:** Buckets without `cors` rules allow every origin in strict mode, for example when a `command` carried over from fake-gcs-server keeps the flag.
- **Breaking change if removed:** Making it a no-op in strict mode changes behavior for anyone relying on it. Changing only the warning breaks nothing.
- **Fixable compatibly after the freeze:** The warning, yes, because wording is not contract. The effect, no.

### Can be fixed compatibly after the freeze

Renames and grammar. These can keep the old name as an alias through 1.x, or add a form, so they do not need to happen before the freeze.

#### 8. Rename `buckets_file_mode`

- **Reason:** The setting also governs the inline `buckets` document. The ready field `buckets_file` has the same misleading name; whether that field can change depends on candidate 4.
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

- The option list. No test compares `Setting` with `--help`, `llms.txt` or `docs/en/native-binary.md`, so a new or renamed option can miss them. The digest test in `src/main.rs` pins only the help text itself.
- Helper help texts. `URL_PUBLIC_HELP`, `URL_SIGN_HELP`, `TEST_SIGNER_HELP` and the `sync` and `import` help have no digest.
- Underscore spellings such as `--foo_bar`.
- `backend=filesystem` from the environment or JSON.
- The ready event's optional field combinations. The ordinary HTTP startup test now pins the complete field set and effective `lifecycle_execution`; other transport/authentication combinations remain separate checks.
- The exit status value. Tests check `success()` or its negation, not the value 1.
- A server started with a non-setting name that the project itself hands out, such as `GCS_MIMIC_SYNC_TOKEN` or `GCS_MIMIC_ENDPOINT`. Tests cover only made-up unknown names.
- The healthcheck's fallback to PID 1's server options. `container_server_args` has no test, and the only probe without `--url` in `scripts/smoke-containers.sh` runs against a container with no server options.
- Image metadata other than the user and data paths. `scripts/smoke-containers.sh` does not check `EXPOSE`, the `HEALTHCHECK` parameters, `SQLITE_TMPDIR` or the licenses path.
- `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS` with an empty value, which `settings_that_define_an_empty_environment_value_start_with_it` omits.
