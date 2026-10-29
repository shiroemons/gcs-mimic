# CORS fallback verification

<!-- markdownlint-configure-file {"MD013": false} -->

Implemented in `5eb3efd` and verified on **2026-10-29** for DESIGN-100 and the
[CORS fallback contract](../slices/cors-fallback.md) with Rust 1.99.1. No
browser, container, SDK or real-cloud run was performed for this slice; the
fallback is a header-level change on routes whose browser behavior is already
covered by the [CORS evidence](cors.md).

## Commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
bash scripts/with-test-tmpdir.sh bash scripts/test-rust.sh
cargo xtask compatibility check
```

## Results

| Check | Result |
| --- | --- |
| Formatting | PASS |
| Clippy, warnings denied | PASS |
| Workspace tests | PASS: 45 test binaries and doctests, 1113 passed, 0 failed, 0 ignored |
| Ledger and generated matrices | PASS |

## Cases

| Test | Behavior |
| --- | --- |
| `config::tests::cors_fallback_defaults_to_none_with_precedence_in_both_profiles` | Default `none`; JSON, environment and CLI precedence; native and legacy-emulator; rejected empty, boolean, underscore, uppercase and `*` values; a bare `--cors-fallback` names `--cors-fallback allow-all` |
| `config_input::tests::legacy_advice_uses_values_precedence_defaults_and_redacts_unsafe_urls` | `-cors-headers` advice starts with removal and the JSON explanation, then names `GCS_MIMIC_CORS_FALLBACK=allow-all` and `--cors-fallback allow-all` only for XML/public browser reads, without echoing the supplied list |
| `xml::cors::tests::allow_all_fallback_allows_any_valid_origin_and_header_only_without_bucket_rules` | Wildcard origin, fixed methods, echoed headers, Location exposure, no credentials or max age; stored rules still deny; PATCH, missing method, malformed headers and origins receive no CORS; `none` adds nothing |
| `xml::cors::tests::unconfigured_warning_is_once_per_bucket_and_bounded` | One warning per bucket name and at most 64 remembered names |
| `allow_all_fallback_covers_unconfigured_xml_buckets_without_overriding_rules_or_json` | Server preflight and GET/HEAD on a bucket without rules, missing-bucket 404, a ruled bucket denying another origin, unchanged JSON credentials and echo, ready `cors_fallback: "allow-all"` and no warning |
| `unconfigured_xml_cors_warns_once_per_bucket_without_the_fallback` | Ready `cors_fallback: "none"`; one `cors_unconfigured` warning across repeated Origin-bearing reads; none for an Origin-less read |
| `process_shutdown_reports_stopped_and_releases_catalog_for_restart` | Ready event key set includes `cors_fallback` |
| `api_origin_ready_and_resumable_locations_use_bound_addresses` | Default ready `cors_fallback: "none"` |

Existing XML, signed and virtual-host CORS tests passed unchanged with the
default `none`.
