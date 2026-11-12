# Option aliases

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Since 0.18.0, source flags are accepted when their translations preserve
requests that worked with fake-gcs-server, including broader native support.
Otherwise startup stops and names the native
alternative. This slice extends UX-003, which stays `PARTIAL`; DESIGN-100's
native CORS fallback remains implemented. The scheme default is `https` up to
0.18.0 and `auto` from 0.19.0; `--public-host` still translates to an `https://`
base without `--scheme`, as fake-gcs-server builds it.

## Behavior

Every server option takes one or two hyphens and a value after a space or `=`.
A command line whose first argument is an option runs `serve`. These spelling
rules have applied since 0.15.0. Words in a native option name are joined by
hyphens: an underscore spelling such as `--max_connections` worked by accident
of name normalization until 0.20.0, and now stops startup with one `cli`
diagnostic, keyed as supplied, that names the hyphenated option. `FAKE_GCS_*`
variables are not read.

| Flag | Kind | Native form or action |
| --- | --- | --- |
| `host` | Native | `--host IP`; changes only the listen IP; `localhost` becomes `127.0.0.1` |
| `port` | Native | `--port N`; changes only the listen port |
| `port-http` | Native | `--port-http N`; requires `--scheme both` |
| `scheme` | Native | `--scheme auto\|http\|https\|both`; default `auto` (`https` up to 0.18.0) |
| `log-level` | Native | `--log-level debug\|info\|warn\|error`; `warning` aliases `warn` |
| `backend` | Native or accepted | `memory` and `disk` are native; `filesystem` becomes `--backend disk` |
| `public-host` | Accepted | `--public-base-url SCHEME://HOST`; see the routing difference below |
| `data` | Accepted | `--seed-dir PATH`; read-only fixtures replay on startup |
| `cert-location` | Accepted | `--tls-cert PATH`; supply a matching key |
| `private-key-location` | Accepted | `--tls-key PATH`; supply a matching certificate |
| `event.pubsub-project-id` | Conditional | `--event-pubsub-project-id ID` |
| `event.pubsub-topic` | Conditional | `--event-pubsub-topic ID` |
| `event.bucket` | Conditional | `--event-bucket NAME` |
| `event.object-prefix` | Conditional | `--event-object-prefix PREFIX` |
| `event.list` | Conditional | `--event-list finalize,delete,metadataUpdate,archive` |
| `filesystem-root` | Refused | Choose native `--data-dir`; convert source data with `gcs-mimic import fake-gcs-server` |
| `external-url` | Refused | Choose `--api-base-url URL`; it also fixes resumable upload URLs |
| `cors-headers` | Refused | Remove it; choose `--cors-fallback allow-all` explicitly if needed |
| `location` | Refused | Choose native `--default-location`; it affects only new buckets without an explicit location |

All `event.*` flags require a configured `PUBSUB_EMULATOR_HOST` or native
`pubsub_emulator_host`, resolved with CLI > environment > JSON precedence.
An explicit empty higher-priority host disables a lower-priority host.
Without one, every event flag is refused: fake-gcs-server can publish to
Google, but gcs-mimic never does. Project and topic must still be complete.
`event.list=none` is refused because it is not a source event selection; use
native `--event-list none` to disable the global rule deliberately.

Accepted translations produce one `info` diagnostic with key
`native_equivalent`. Its `native_args` JSON string array is the canonical
equivalent, preserving paths, identifiers, prefixes and enum values. Decode
the array and pass each element as one process argument. For example, extract
the array with jq:

```sh
jq -c '.diagnostics[]? | select(.key == "native_equivalent") | .native_args' server.log
```

When no value contains a single quote, double quote, backslash or control
character, `fix` also ends with a POSIX-shell-quoted line after
`Use native options:`. That line can be copied directly from raw JSON stderr.
Otherwise no shell line is emitted: JSON escaping would change its values,
so the diagnostic directs users to `native_args`. URLs with credentials,
queries, fragments or invalid syntax are redacted; invalid public origins
fail validation before startup. `log_level=warn` or `error` hides this info.

Every refused flag produces an `error` diagnostic keyed by its original
spelling. All refusals are reported together, with exit status 1 and no ready
event or catalog creation. Validation warnings and errors remain visible at
every log level. Native and accepted values retain normal native validation.
The last occurrence of a translated flag wins. A translated setting overrides
a native setting from the same CLI and warns about that conflict; source
precedence remains unchanged.

## Why these flags stop startup

`filesystem-root` refers to a different storage format. The image's native
data path is `/var/lib/gcs-mimic`; a stopped source snapshot must be imported
into a new destination. Previously the flag was ignored with a warning.

`external-url` sets only source `selfLink` and `mediaLink` bases; resumable
upload `Location` follows the request address. Native `api_base_url` fixes
resumable URLs too. Choose `--api-base-url` only with an address every client
can reach, or omit it to derive links from requests.

`location` is a server-wide response location in the pinned source. Native
`default_location` affects only new buckets without an explicit location;
existing buckets and explicit values win. The legacy profile keeps its fixed
location and rejects an explicit native default.

`cors-headers` adds a global request-header allowlist in the source. Mapping
it to `allow-all` discarded that list. Permissive mode already allows every
origin on XML buckets without rules; strict mode follows bucket CORS rules.
Native `--cors-fallback allow-all` is the explicit opt-in for unconfigured
XML/public buckets. Stored bucket rules remain exact in either mode, and
JSON preflight already permits every valid requested header.

These decisions use the pinned source described in the
[configuration reference](../fake-gcs-server-configuration.md). They do not
claim whole-server parity for storage, authorization or event delivery.

## public-host routing difference

`public-host` is accepted as `public_base_url` with the effective scheme.
Native path-style accepts more hosts than the source, and IP authorities are
valid. Public-host subdomains select virtual-host buckets: call the JSON API
through a host outside those subdomains or set `--api-base-url` explicitly.
An explicit API origin that conflicts with a virtual-host bucket still fails
the existing startup validation. No request routing is changed by this slice.

## Unchanged command behavior

Helper commands reject server flags and point to the setting for `serve`.
The healthcheck first probes the URL derived from environment and file
settings. Only after that fails, and PID 1 is gcs-mimic running server options,
does it derive a second URL from PID 1's arguments. Reading PID 1 only after a
failure matters on Apple Silicon: Rosetta emulates `/proc/<pid>/cmdline` for
an amd64 image with a temporary file, so reading it on a read-only root crashes
the probe. A server-option `command` on a read-only amd64 container under
Rosetta retains this limitation.

## Acceptance and verification

| Behavior | Test |
| --- | --- |
| Every flag has an explicit kind; four CLI spellings and redaction | `config_input::tests::alias_inventory_and_all_flag_forms_are_diagnosed_even_when_empty` |
| Native listener options start both sockets; unused HTTP port fails | `native_listener_flags_start_both_listeners_and_reject_unused_http_port` |
| Empty listener environment preserves defaults for CLI listener parts | `empty_listen_environment_keeps_defaults_when_a_cli_part_overrides_it` |
| Public hosts accept DNS and IP origins; explicit conflicts still fail | `public_host_accepts_dns_and_ip_origins_and_preserves_explicit_api_conflict_errors` |
| Invalid listener, public-host and TLS-path values retain original keys | `invalid_native_and_translated_flag_values_keep_the_original_key` |
| All four refused flags fail together in both auth modes and profiles | `refused_flags_report_every_original_key_before_startup` |
| Event flags honor CLI, both environment names, JSON and empty overrides | `event_aliases_require_an_effective_local_emulator_host` |
| Native argument array reproduces seed, TLS connections and delivered events | `accepted_native_args_round_trip_through_tls_and_notifications` |
| Safe shell line copied from raw stderr preserves values | `safe_native_line_round_trips_from_raw_stderr` |
| Quotes, backslashes, newlines and tabs use exact array values without a shell line | `json_escaped_values_use_exact_native_args_without_a_pasteable_line` |
| Source-invalid event selection and invalid TLS material fail | `alias_values_that_have_no_source_meaning_or_invalid_tls_stop_startup` |
| Info obeys log level; validation warnings/errors remain visible | `log_levels_filter_runtime_events_and_preserve_lifecycle_and_input_warnings`, `invalid_log_levels_and_alias_diagnostics_fail_before_catalog` |

On 2026-11-11, the final source passed:

- `cargo check --tests`.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`.
- `cargo test --locked --offline --lib config_input::tests`: 22 passed.
- `cargo test --locked --offline --bin gcs-mimic`: 32 passed, including
  the help digest.
- Combined process, XML/auth and import/seed suites: 100, 317 and 94 passed,
  with no failures, using the command below.
- `cargo fmt --check` and `cargo xtask lint`: no new production violations
  or ratcheted findings.
- `bash scripts/check-docs.sh` and `just check-markdown`.

```sh
cargo test --locked --offline \
  --test it_process --test it_xml_auth --test it_import_seed -- --test-threads=4
```

The shell and special-path round-trip tests run on Unix. The suites cover the
original location refusal key and advice, trusted custom TLS, actual local event
delivery and the shared public-host XML/signature fixture.
Whole-repository tests, container builds, SDK harnesses and guide walkthroughs
await final integrated-source verification; they were not rerun after this change.
