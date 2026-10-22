# JSON byte-range verification

Verified **2026-10-22** on macOS arm64 at implementation commit
`f1e258096225adf98167ad02abb89b1a722961bd`.
The [slice contract](../slices/json-object-range.md) contains the sources,
acceptance cases, proposed ledger changes and explicit local rules.
No shared ledger or generated matrix was modified.

## Executed checks

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --test object_range_http -- --nocapture` | PASS: five wire tests during implementation; all five also pass in the final full check |
| `cargo test --locked --offline --lib object_range_reader` | PASS: sparse-reader test during implementation; its final bounded 64 MiB variant also passes in the full check |
| `just check` | PASS, exit 0: formatting, clippy with `-D warnings`, 229 workspace tests, compatibility check of 591 feature rows and existing shell/mock checks |
| `GCS_MIMIC_GO_TEST_RUN=TestObjectRangeReader bash scripts/sdk-go.sh` | PASS, exit 0: vet and official JSON range-reader test with five named positive subtests, at/past-end failures and a failed generation condition |
| `cargo xtask dependencies check` | UPDATE DETECTED, exit 1: Rust/direct crates current; Storage v1.69.0 -> v1.70.0; module updates belong to the separate dependency integration |
| `git diff --check` | PASS, exit 0 before the code commit |
| Local-path scan with `git grep -nE` | No matches before the code commit |
| `npx --yes markdownlint-cli2` with the table-length exemption described below | PASS for the slice contract and evidence summary |

Rust is `1.99.1` (edition 2024), Cargo is `1.99.1` and locked application
dependencies were used. The Go module selects `go1.27.3` via
`GOTOOLCHAIN=auto`; the bootstrap executable outside the module is older.
The tested module pins Storage `v1.69.0`, API client `v0.307.0` and oauth2
`v0.37.0`. The harness uses `storage.WithJSONReads()` and isolated permissive
and strict servers; only `TestObjectRangeReader` runs under this filter.

[Rust/check excerpts](object-range-checks.txt) and
[Go output](object-range-go.txt) retain one final evidence set. Machine-local
path prefixes are redacted; the check file is explicitly an excerpt of the
successful final run, with selected original lines left unchanged.
The existing shell/mock container-publication tests inside `just check` do not
build or run container images.

Markdown lint retains the default rules with only `MD013.tables=false` in a
temporary configuration. The tables contain exact test identifiers and header
contracts that cannot fit within 80 columns; prose still uses the default limit.

## Verified behavior and limits

- Both JSON media prefixes deliver closed, open-ended, suffix and clamped ranges
  with 206, exact Content-Range/Length and generation-bound representation headers.
- Unsatisfiable offsets/zero suffixes deliver the documented JSON 416 reason and
  RFC length indication. Empty nonzero suffixes use the documented local 200
  fallback; malformed, unknown-unit and multi/repeated ranges also return full 200.
- Failed match/not-match conditions precede Range evaluation. Strict anonymous
  and insufficient-role requests fail before either Range or If-Range handling.
- The overwrite wire test waits for the original 206 headers, overwrites the
  object and then checks every byte of the original 8 MiB-minus-one range.
  A fresh old-generation request fails 404 because history is not retained.
- The 64 MiB sparse-file test seeks before reading, emits 65536 then 3 bytes,
  stops at the requested cursor, then verifies a two-byte limit and EOF error.
  This establishes reader bounds; no RSS/CPU/throughput benchmark was performed.
- Gzip ranges address stored compressed bytes with gzip acceptance or
  no-transform. Required decompression and If-Range remain explicit 501 errors.
- Full-object hashes stay full-object hashes. Complete production partial/error
  header parity, malformed/multi-range responses and empty-suffix details have
  not been verified against a real Google bucket.

No real-cloud differential test, container build, container SDK run, XML/gRPC
range test or exact getting-started curl invocation was performed. The
integration owner must update ledgers/views and verify any newer SDK pin.
