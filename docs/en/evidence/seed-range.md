# Byte-range and initial seeding integration

Verified on **2026-10-22**, after integrating the range and seed implementations.
The [ledger](../../../compatibility/features.json) remains the status authority.

- BEHAVIOR-OBJECT-016 is IMPLEMENTED for documented single untranscoded JSON
  ranges, including bounded seeking, preconditions and generation pinning.
- JSON-OBJECTS-005 and DESIGN-082 remain PARTIAL. If-Range, transcoding,
  fallback parity and exhaustive partial-header parity remain explicit gaps.
- UX-002 remains PARTIAL: the macOS native cases pass; Linux invalid-byte
  filename rejection and the seeded Compose journey have not been executed.
- UX-008/UX-009 retain their partial status and refer seed marker/reset scope
  to UX-002. No new seed specification gap is proposed.

| Check | Result | Output |
| --- | --- | --- |
| `cargo +1.99.1 build --release --locked --bin gcs-mimic` | PASS | Release binary built from the final integrated source |
| `just check` | PASS; fmt, clippy with warnings denied, all workspace tests, ledger check and offline shell/release checks | [Check output](seed-range-checks.txt) |
| `just check-sdk` | PASS; full native Go suite, including `TestObjectRangeReader` | [SDK output](seed-range-sdk.txt) |
| New native seed/range guide shell block executed with `bash -euxo pipefail` | PASS; fresh catalog, seed-before-ready, exact full bytes and 206 `bytes 0-4/11` with body `hello` | [Guide output](seed-range-guide.txt) |
| `cargo xtask compatibility render` and `cargo xtask compatibility check` | PASS; both generated matrices agree with the ledger | Included in final ledger checks |
| markdownlint-cli2 v0.24.0 on changed Markdown | PASS | MD013 disabled for long existing tables/URLs; MD024 uses `siblings_only=true` for generated category headings |

Go inputs are 1.27.3, Storage v1.70.0 and API client v0.307.0; the full SDK run
uses the committed module graph. Historical v1.69.0 evidence is unchanged.
The staged whitespace check exited 2 on the raw guide log's HTTP CRLF headers,
for example `seed-range-guide.txt:40: trailing whitespace.` with
`+HTTP/1.1 206 Partial Content` followed by a carriage return. Those bytes are
retained as actual curl output. The documentation/ledger whitespace check,
excluding raw TXT evidence, passed.
Source review checked the official [JSON headers](https://docs.cloud.google.com/storage/docs/json_api/v1/parameters#range)
and [416 status](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes#416_requested_range_not_satisfiable)
references on 2026-10-22. See the [range contract](../slices/json-object-range.md)
and [seed contract](../slices/seed.md) for the scoped acceptance mappings.

Container builds and container smoke were **not run in this integration session**.
The inspected existing native-architecture image's help had no seed options, so
it could not verify the integrated implementation without a new build. Neither
`just check-containers` nor `just pre-push` was run. No real cloud requests,
power-loss tests or resource benchmarks were run. The next slice is bucket IAM
and public reads / XML GET.
