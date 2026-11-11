# Public contract review for the 1.0.0 freeze

This document supports step 1 of the [1.0.0 gate](release-policy.md#the-100-gate), "Freeze the public contract". It inventories the stable surface and proposes changes for the project owner to decide. The owner has decided candidates 1, 2, 4, 5, 6 and 7, and the `scheme` and `data_dir` rows of candidate 3. Candidate 1 is implemented. The other decided candidates list the changes the code, examples, documents and tests still need before the freeze, and the points each decision leaves open. The remaining rows of candidate 3 and candidates 8 to 17 are undecided proposals.

While the project is in `0.x`, a minor release may make breaking changes, so the breaking changes decided here ship in `0.18.0` without a transition period. The deprecation procedure for `1.x` is decided at the freeze.

Reviewed source: version `0.17.1`, commit `a53a1f8c`, on 2026-11-10. The decisions of 2026-11-11 were checked against version `0.17.1`, commit `10266125`.

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
5. The `--cors-headers` warning says "strict mode follows each bucket's cors rules". The flag sets `cors_fallback=allow-all` in strict mode too (observed: ready reports `allow-all`), which contradicts the warning and the comment in `alias_advice`. The owner decided that the flag stops startup in every mode; see candidate 7.
6. S `option-aliases.md` says `public-host` warns that Host matching needs the legacy profile. The `serve` warning (`alias_difference`) does not mention the profile; only the helper-command advice (`alias_advice`) does.
7. N says `buckets` and `identities` have the "same limits" as their file forms. In a JSON config file they are bound by that file's 64 KiB limit (`read_config`).
8. `--event-list` help says `none` disables the global rule. Alone it fails startup, because event filters need a project and topic (`src/config/notifications.rs`; observed).
9. C's list of settings that define an empty value omits `GCS_MIMIC_TRUSTED_ALLOWED_HOSTS`, which the code accepts empty when `trusted_listen` is set (`src/config.rs` `allowed_hosts`).
10. L lists the bucket entry's optional fields as `cors`, `iamPolicy`, `versioning` and `softDeletePolicy`. The code also accepts `project`, `location`, `storageClass`, `labels`, `defaultEventBasedHold` and `iamConfiguration` (`src/bucket_init.rs` `Entry`).
11. The main help lists `sync findings` and `sync deletions`, and the code accepts them, but `sync --help` shows only `status --view`. `sync approve` also accepts `--revision`, which its help omits; its error message names `--revision` while the help names `--deletion-set`.
12. `url sign --help` lists GET, HEAD, PUT and DELETE. The code also signs POST when `x-goog-resumable: start` is supplied (`src/auth/signature.rs`).
13. The main help's usage line omits `--h` and `-version`, which the code accepts.
14. Resolved: release tooling accepts `MAJOR.MINOR.PATCH-rc.N` with positive `N`, publishes only its immutable exact container tag and marks its GitHub Release as a non-latest prerelease. Stable versions alone determine floating tags. Each candidate is a supported upgrade source with its own released-data fixture; see candidate 1 and the [release policy](release-policy.md).
15. `--memory-max-bytes` help and L say half of available memory. The code subtracts 32 MiB first, and it uses 1 GiB wherever it cannot read available memory, which is everywhere except Linux (`src/memory_limit.rs`).
16. S `runtime-admin-endpoints.md` and S `local-instance-reset.md` say management bodies are capped at 64 KiB. The handlers read at most `RESOURCE_BODY_LIMIT`, which is 16 MiB (`src/json/body.rs`, `src/server/admin.rs`, `src/server/admin/reset.rs`). The owner decided that 64 KiB is the guaranteed size and larger bodies are not promised, so the documents change rather than the code; see candidate 6.
17. H says server CLI overrides need an explicit `--url` or matching environment or configuration settings. The code falls back to PID 1's server options when the first probe fails ([healthcheck URL selection](#healthcheck-url-selection)), as C says.

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
  - `scheme`: the default becomes `http`. Only `compat_profile=legacy-emulator` keeps the reference emulator's `https` default, whether it comes from the flag, the environment or the JSON file. Using fake-gcs-server flags alone does not change the default. When a fake-gcs-server flag is used, `scheme` is not set and the effective scheme falls to the `http` default, startup says so: the default is `http`, and `scheme=https` or `compat_profile=legacy-emulator` matches the reference emulator.
  - `data_dir`: the default `data` stays. Startup reports the data directory it opened and whether it created a new catalog or opened an existing one.
  - The other rows of the table (the settings that follow the authentication profile, `buckets_file_mode`, the `latest` image tag and the boolean spellings) are not decided and stay proposals for the freeze.
- **0.x defaults (2026-11-11, revisable before the freeze):**
  - When both `tls_cert` and `tls_key` are set and `scheme` is not, the scheme is `https`, because supplying a certificate states the intent. One of them alone, or either with `scheme=http`, stays the current error.
  - The data directory is reported as an absolute path, joined with the working directory but not canonicalized, in a ready-event field and in an `info` runtime event (`diagnostics::emit`). With `backend=memory`, which never opens `data_dir`, the implementing slice either omits the field or reports a value that says so, and records the choice in this document and in `docs/en/native-binary.md`.
- **Required change:** every item ships in `0.18.0`.
  - **Breaking:** the `scheme` default. `Config::default` in `src/config.rs` sets `Scheme::Https`, and configuration stores the scheme as a plain value, so nothing can tell an explicit `https` from the default. Resolution must see whether `scheme` was set, the effective `compat_profile` and whether both TLS paths were given, from every source: the command line, the environment and the JSON file are all read before `collect_settings` (`Config::preflight_os`). The resolved scheme then reaches every place that derives a URL: the listeners, `healthcheck_url` in `src/config.rs`, and the healthcheck's PID 1 fallback (`container_server_args` and `probe_url` in `src/main.rs`). Step 1 of the [healthcheck URL selection](#healthcheck-url-selection) reads only the environment and the JSON file, so a server given `--compat-profile legacy-emulator` or both TLS flags on its command line serves HTTPS while step 1 probes HTTP; the fallback then finds it through PID 1's options. `public-host` uses an explicit `scheme` and otherwise `https` (`TranslationBase::public_scheme` in `src/config_input.rs`); it must use the resolved scheme.
  - **Breaking:** `tls_handshake_timeout_ms` without `scheme` and without both TLS paths becomes an error, because that setting is an error with `scheme=http` (`src/config.rs`). The release notes say to add `scheme=https`.
  - **Breaking:** `compose.yaml` sets no scheme and relies on the HTTPS default; `docs/en/local-https.md` says the image and `compose.yaml` default to built-in HTTPS. Either `compose.yaml` sets `GCS_MIMIC_SCHEME: https` or the document changes to HTTP. `compose.tls.yaml` sets `https` and both TLS paths and is unaffected.
  - The startup notice for a fake-gcs-server flag without `scheme`: a warning in the startup diagnostics, shown with the native spelling block of candidate 5. Wording is not contract.
  - Help and documents that state the `https` default: the `HELP` constant in `src/main.rs` (its digest test must be updated with it), the "No command given" advice in `src/main.rs`, which suggests `serve --scheme http`, `llms.txt` (the option table and the troubleshooting row for "Received HTTP/0.9"), `README.md`, `docs/en/native-binary.md`, `docs/en/container-usage.md`, `docs/en/local-https.md`, `docs/en/getting-started.md`, `docs/en/fake-gcs-server-migration.md` (the `-scheme http` row and the listener row), `docs/ja/container-usage.md` and `docs/ja/fake-gcs-server-migration.md`. The examples set `GCS_MIMIC_SCHEME: http` explicitly and keep working; dropping that line is optional.
  - Tests that pin the HTTPS default: `tls_configuration_precedence_and_validation` in `src/config.rs`, `fake_gcs_server_variables_are_not_read` in `src/config_input.rs`, and `healthcheck_default_url_maps_wildcards_and_ignores_advertised_origin` in `tests/it_process/healthcheck_defaults.rs`, which builds its configuration from `Config::default`. New tests: startup without `scheme` listens on HTTP; `compat_profile=legacy-emulator` from each source listens on HTTPS; both TLS paths without `scheme` listen on HTTPS, and one path alone fails; an explicit `scheme` wins in every case; a fake-gcs-server flag without `scheme` gives the notice and HTTP; the healthcheck finds a server started each way without `--url`.
  - Additive: the `data_dir` report. `Catalog::open_for_server` in `src/storage/catalog/open.rs` knows whether it created a catalog (a stored schema version of 0 runs `initialize_catalog`) but does not return it, and `ReadyEvent` in `src/startup.rs` has no data directory field. The open path returns both facts, `src/startup.rs` puts them in the ready event and emits the `info` event, and a process test pins them for a new directory, an existing one and the memory backend. `docs/en/native-binary.md`, `docs/en/container-usage.md` and the ready-event row of this document list the new fields. The field names are contract under candidate 4 once released.
- **Open points for the freeze:**
  - Whether the two 0.x defaults above stay.
  - The undecided rows of the table above.

#### 4. Make machine-readable names and exit codes contract, not wording

- **Reason:** The policy excludes diagnostic wording, but it does not say whether ready fields, diagnostic keys, event names or exit status are stable. `protocol` is always `http1` and `gcs_api` is a fixed string that predates XML and gRPC, though a test pins both (`tests/it_process/foundation.rs`).
- **Cost of keeping it undecided:** Every field rename in 1.x becomes an argument, and stale values must stay forever if the ready event is contract.
- **Breaking change if removed:** Dropping or renaming a ready field breaks scripts that read it, such as the `jq` examples in N. Declaring the fields unstable breaks nothing but withdraws an implied promise.
- **Fixable compatibly after the freeze:** Partly. Promising more later is compatible; withdrawing a promise is not, and stale values can be dropped only before they are promised.
- **Owner decisions (2026-11-11):** Ready-event field names, diagnostic keys, event names, exit codes and the response fields of the user-facing emulator routes (candidate 6) are contract. Wording is not. The stale ready fields `protocol` and `gcs_api` are removed in a `0.x` release before the freeze.
- **Required change:** the removal ships in `0.18.0`; the documents and tests can follow in any release before the freeze.
  - **Breaking:** remove `protocol` and `gcs_api` from `ReadyEvent` in `src/startup.rs`, and from the expected field list and the value assertions in `tests/it_process/foundation.rs`. No document, example or SDK harness reads either field; the release notes name the removal. The ready-event row of [exit codes and machine-readable output](#exit-codes-and-machine-readable-output) drops them.
  - [Release policy](release-policy.md): states which machine-readable output is contract. That statement is true of the code today and does not wait for the removal.
  - Documents: an inventory of what is now contract. `docs/en/native-binary.md` names individual ready fields in examples and option rows, and only this document lists them all. Diagnostic sources are documented only partly (F), and runtime event names exist only as string literals at each `diagnostics::emit` call site in `src/`, with no list in code or documents.
  - Tests for what [has no guard](#contract-areas-without-a-test-guarding-them): the exit status value 1, the ready field sets of the other transport and authentication combinations, and a list of event names that fails when one is renamed or removed.
- **Open points for the freeze (outside `0.18.0`):**
  - Whether "diagnostic keys" covers only the member names of a diagnostics line (`event`, `level`, `diagnostics`, `source`, `key`, `fix`) or also the values of `source` and `key`. The `key` value is the input name as supplied, such as `GCS_MIMIC_LOG_LEVEL`, `-cors-headers` or `data_dir`.
  - Whether the fields of runtime events other than `event` and `level` are contract, and whether the import output (`import_page`, `import_complete` and their fields) counts as events.
  - Whether ready-field values are contract as well as names, for example the strings that `transport`, `backend` and `auth_mode` report.
  - The `gcs-mimic sync` output. The command writes the sync route's JSON response to stdout unchanged (`src/sync_cli.rs`), and candidate 6 places the sync routes outside the contract, so the output fields of a documented command are not covered by either decision.

#### 5. Accept a fake-gcs-server flag only when it translates exactly

- **Reason:** Seventeen migration flags would be frozen with their exact translations, including `filesystem-root`, which is accepted and ignored.
- **Cost of keeping them:** Each flag and its value handling must survive every 1.x release, and each one adds to the CLI parser's special cases.
- **Breaking change if removed:** Removing a flag stops command lines copied from fake-gcs-server. A policy line that makes aliases deprecatable with notice breaks nothing now.
- **Fixable compatibly after the freeze:** No. Once the flags are inside the contract, removing one or excluding them later withdraws a promise.
- **Owner decisions (2026-11-11):**
  - `--host`, `--port` and `--port-http` are native options and part of the stable contract, like `--scheme` and `--log-level`.
  - The other flags in the [alias table](#alias-flags) come from fake-gcs-server and follow one rule: a flag whose meaning translates exactly to native settings is accepted, and one that does not stops startup. Either way startup shows the native spelling. For accepted flags, startup prints the native replacements in one block.
  - `filesystem-root`, which is ignored today, and `cors-headers` (candidate 7) stop startup. `public-host`, the `event.*` flags and `backend filesystem` are checked one by one when the rule is implemented, and each that cannot translate exactly also stops startup.
  - Accepted fake-gcs-server flags are outside the stable contract. The deprecation notice period is decided at the freeze.
- **Required change:** every item ships in `0.18.0`.
  - **Breaking:** `filesystem-root` stops startup. Today `translation` in `src/config_input.rs` returns no setting for it and `alias_difference` warns. The error shows the native spelling: mount the data volume at `data_dir` and convert old data with `gcs-mimic import fake-gcs-server`. Tests that use it as an ordinary warning change: `tests/it_process/log_level.rs` and the startup-failure test in `tests/it_process/foundation.rs` that passes `-filesystem-root /storage` next to an invalid setting. `scripts/compare-reference-emulator.sh` passes it only to the reference emulator and is unaffected.
  - **Breaking:** each of `public-host`, `event.*` and `backend filesystem` that the implementing slice finds inexact stops startup in the same way. Today `public-host` warns that Host matching needs the legacy profile only in helper advice ([disagreement 6](#disagreements-between-code-and-documents)); the `event.*` flags warn that only local emulator delivery is enabled; `backend filesystem` becomes `backend=disk`. The slice records each finding and its test in this document and in `docs/en/slices/option-aliases.md`.
  - `src/config_input.rs`: `AliasFlag` today mixes the native listener flags (`Host`, `Port`, `PortHttp`), the native `Scheme` and `LogLevel`, and the fake-gcs-server flags. The native flags must not trigger the fake-gcs-server notice or the `scheme` notice of candidate 3. The per-flag warnings of `alias_difference` become one block of native replacements, and helper commands keep rejecting the fake-gcs-server flags with advice (`alias_advice`). The block and the advice are wording, so they are not breaking.
  - `src/main.rs`: the `HELP` constant already lists `--host`, `--port` and `--port-http` with the native options and does not list the fake-gcs-server flags; it changes only if the slice adds a pointer to the migration guide (the digest test follows).
  - Documents: the [alias table](#alias-flags) of this document, `docs/en/slices/option-aliases.md`, `docs/en/fake-gcs-server-configuration.md`, `docs/en/fake-gcs-server-migration.md`, `docs/en/migrate-from-fake-gcs-server.md`, `docs/en/fake-gcs-server-storage.md`, `docs/en/container-usage.md`, `docs/en/slices/configuration-workflows.md`, `docs/en/slices/source-storage-contract.md`, `llms.txt`, and the Japanese `fake-gcs-server-configuration.md`, `fake-gcs-server-migration.md`, `migrate-from-fake-gcs-server.md`, `container-usage.md` and `storage-backends.md`, wherever they describe a flag that changes. `docs/en/native-binary.md` and `README.md` already present `--host`, `--port` and `--port-http` as native.
  - Tests: each accepted flag starts the server with exactly its native settings and prints the block; each stopped flag exits 1 with the native spelling; the native listener flags print neither the block nor a notice.
  - Ledger: `UX-003` and `DESIGN-100` in `compatibility/features.json` record alias behavior; the slice updates the affected acceptance cases with its evidence.
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
- **Required change:** ships in `0.18.0`.
  - **Breaking:** `translation` in `src/config_input.rs` maps `CorsHeaders` to `cors_fallback=allow-all`, and `alias_difference` warns. Both go; the flag becomes a startup error in permissive and strict mode alike, which also removes the case where the flag overrides an explicit `GCS_MIMIC_CORS_FALLBACK=none` from the environment or JSON. Helper commands keep rejecting the flag with advice (`alias_advice`), which names `cors_fallback=allow-all`.
  - Tests in `src/config_input.rs` that expect a warning with "remove this setting", including the one whose comment says the alias still selects `allow-all`, expect the error instead. New tests: the flag exits 1 in both modes with the native spelling, and an explicit `cors_fallback=allow-all` without the flag starts in both modes.
  - Documents: [disagreement 5](#disagreements-between-code-and-documents) and the alias table in this document; the `cors-headers` row of `docs/en/slices/option-aliases.md`; `docs/en/slices/cors-fallback.md`; `docs/en/fake-gcs-server-configuration.md`, `docs/en/fake-gcs-server-migration.md`, `docs/en/migrate-from-fake-gcs-server.md`, `docs/en/fake-gcs-server-storage.md`, `docs/en/container-usage.md` and their Japanese companions where they describe the flag; and the release notes.
  - Ledger: the `DESIGN-100-CONFIG` acceptance case of `DESIGN-100` in `compatibility/features.json` says that "the alias still selects allow-all"; the implementing slice updates it with its evidence.
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
