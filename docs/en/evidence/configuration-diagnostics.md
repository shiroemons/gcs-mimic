# Configuration migration diagnostics evidence

<!-- markdownlint-configure-file {"MD013": false} -->

Verified on **2026-10-24** with Rust **1.99.1**, edition **2024**, on
macOS arm64. The implementation was verified at pre-consolidation source
`56d156c` and the contract is
[configuration workflows](../slices/configuration-workflows.md). This is a
local process/configuration feature; DESIGN-055 and UX-003 remain PARTIAL, and
the independently owned contract-review cases remain PENDING.
Source labels in this record identify executions before history consolidation.
The final `src` tree is `481a851b6bbb47e8cb3f5dfb4d2afa18f1c5e2f2` and
the final `tests` tree is `fe4eaec074b69c8b8bb4675456a490f7763e509e`;
`git rev-parse HEAD:src HEAD:tests` verifies them independently of the final
squashed commit ID.

## Verified behavior

| Coverage | Evidence |
| --- | --- |
| All 19 source variables, including empty values; four CLI spellings; native `--scheme` collision | `config_input::tests::legacy_inventory_and_all_flag_forms_are_diagnosed_even_when_empty` |
| Effective legacy CLI/environment/default advice, IPv6, safe proxy URL, public/API authority warning, CORS distinction, unsupported future settings and URL/secret redaction | `config_input::tests::legacy_advice_uses_values_precedence_defaults_and_redacts_unsafe_urls` |
| Mixed equals/separate flags, missing subcommand/Compose CMD replacement, unknown single-hyphen flags, native value positions and unrelated `FAKE_GCS_CONFIG` | `config_input::tests::option_positions_preserve_native_values_and_equals_do_not_shift_tokens` |
| 1024/1025 occurrences, 256/257-byte UTF-8 keys, duplicate JSON occurrence counting and combined source limit | `config_input::tests::supplied_occurrences_and_original_utf8_keys_are_bounded`; `config::tests::config_occurrence_limits_count_duplicate_json_keys_and_all_input_sources` |
| `/data` nonempty/empty/missing/inspection failures, disjoint/equal/ancestor/descendant paths, symlink aliases/dangling aliases and missing destination ancestors | `config_input::tests::data_inspection_is_bounded_conservative_and_resolves_missing_ancestors` |
| Arbitrary Unicode CLI/environment inputs produce no parser panic and valid JSON | `config_input::tests::arbitrary_legacy_and_native_inputs_never_panic`, proptest 1.12.0, 256 cases per normal run |
| Stable source/key/repeated-key ordering, control-character escaping, warn before error, separate pages without replay, operational-source names | `diagnostics::tests::batches_are_ordered_escaped_independent_and_do_not_replay_pages` |
| Independent native errors, precedence, invalid-prerequisite suppression | `config::tests::effective_validation_aggregates_without_cascades_and_keeps_native_precedence` and existing config tests |
| Unknown legacy non-UTF-8 values warn without blocking ready; known/native invalid encodings join independent CLI errors without exposing bytes; effective native overrides remain valid | `non_utf8_environment_values_preserve_unknown_warnings_and_aggregate_errors`; `config_input::tests::native_environment_encoding_follows_effective_precedence` |
| An invalid handshake timeout does not suppress independent PEM and identity inspection | `invalid_tls_timeout_does_not_hide_independent_tls_and_identity_failures` |
| Native equals/missing-value syntax errors retain invalid prerequisites and suppress dependent TLS-pair/auth-profile errors; three-or-more hyphens are rejected without touching native option values | `config::tests::native_syntax_errors_preserve_invalid_prerequisites_without_cascades`; `config_input::tests::option_prefixes_require_exactly_two_native_or_one_or_two_legacy_hyphens` |
| Malformed config plus invalid native CLI/environment plus failed TLS and identity all appear in one startup batch; no raw file values, local paths, ready or data creation | `startup_diagnostics_aggregate_independent_inputs_and_material_without_side_effects` |
| Unknown legacy environment warns and still becomes ready; three Compose-only variables fail; flag-like object values succeed in public and signed helpers | `unknown_legacy_environment_warns_and_native_server_still_becomes_ready`; `compose_only_variables_and_missing_subcommand_report_native_migration`; `native_helper_values_that_look_like_legacy_flags_are_preserved` |
| Existing strict/permissive profile validation, seed failures/recovery, public/signing helper rejection, and single-listener TLS trust/material behavior | `auth_http` (93), `seed` (11), `url_cli` (7), `sign_url_cli` (4), `tls` (9) tests all passed |

Focused commands passed: `cargo test --lib config --locked --offline`
(19 tests), `cargo test --lib diagnostics --locked --offline` (2 matching tests),
`cargo test --test foundation --locked --offline` (19 tests), the five regression
targets above, and `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`.
The config, input parser and diagnostic modules deny `unwrap_used`/`expect_used`;
their assertion conveniences are allowed only in test modules. The proptest
addition required one explicit error type in the existing stored-policy parser;
it does not change policy behavior.

The full `just check` run at source **`2d95163`** passed:
**686 tests, zero failures and zero ignored**,
plus formatting, Clippy with denied warnings, ledger checks and the offline
shell/container-publication recipe checks. These script checks do not build a
container or publish anything. The
[complete workspace check log](configuration-diagnostics-checks.txt) replaces only
the checkout path prefix. Ledger validate/render/check then passed for
591 feature rows, 401 parameter locations and 357 schema fields; regenerating
the English/Japanese views produced no tracked changes because parent feature
statuses remain unchanged.
After the input-boundary corrections in **`5b1c387`**, the final
[focused check log](configuration-diagnostics-focused.txt) records **19 config
tests and 19 foundation tests**, including every required existing startup/helper
regression. Clippy with denied warnings and the existing strict-profile startup
test also passed. The subsequent **`56d156c`** refinement preserves invalid
legacy UTF-8 components as placeholders rather than fabricating defaults;
the focused log also records that process regression and the final Clippy pass.
The full workspace command was not repeated after those
corrections; the integration owner runs it after merging with concurrent tracks.
Markdownlint-cli2 **0.24.0** checked the eight changed Markdown files, including
guides, the contract and this evidence document, with zero issues. Existing per-file MD013 allowances
remain unchanged; no new lint rule was disabled.

## Verification scope and limitations

The input changes intentionally replace 0.1.0's ignored legacy variables in the
development binary for the next minor release. They do not accept aliases,
implement reserved settings, select a compatibility profile, grant permissions,
import data implicitly, or alter catalog/startup ordering. The source event
renderer is reusable per bounded producer page; this slice does not implement
Initializer/Sync/Import producers or global runtime log filtering.

Configuration, material and top-level operational errors use the JSON diagnostic
renderer. Existing non-error/runtime stderr events are still owned by the later
log-level slice. The `/data` inspection test supplies isolated filesystem paths
to the same bounded production inspector; it does not create or modify the
host's literal `/data`. Runtime/signal allocation and simultaneous cleanup
failures were not fault-injected. RSS, cgroup memory, CPU, throughput, disk use
and latency are **unmeasured**. No container build, SDK run, full guide execution,
browser or real-cloud request was needed or performed for this slice.

## Dependencies

The official index confirms latest non-yanked stable **proptest 1.12.0**,
released 2026-10-10, requiring Rust 1.88, under **MIT OR Apache-2.0**. Its archive
is **213842 bytes**, SHA-256
`b8530004ccb15eae51c7e40009fbe317f341f804db54dc033eec1c50be28cfa0`.
The official repository was not archived and reported last push
`2026-10-10T18:18:37Z`. Only `std` is enabled; fork/timeout/bit-set are disabled.
Eleven packages are test-only additions, and existing locked serde 1.0.229 with
derive is promoted without a production package version update. See the
[dependency snapshot](../../../compatibility/dependency-status.json).

Sources checked directly on 2026-10-24:

- [Official proptest Cargo index](https://index.crates.io/pr/op/proptest)
- [Publisher manifest](https://raw.githubusercontent.com/proptest-rs/proptest/v1.12.0/proptest/Cargo.toml)
- [Official crate archive](https://static.crates.io/crates/proptest/proptest-1.12.0.crate)
- [Publisher maintenance metadata](https://api.github.com/repos/proptest-rs/proptest)
- [Official serde Cargo index](https://index.crates.io/se/rd/serde)

The dependency checker reports the existing distroless runtime-image digest
update; it is not fixed or declared current by this slice. Distribution owns
its update and release gate. Rust and the selected direct Rust dependencies
remain current in the checked snapshot.
The [final dependency log](configuration-diagnostics-dependencies.txt) records
exit **1** solely for that image update, including CURRENT results for serde
1.0.229 and proptest 1.12.0.
