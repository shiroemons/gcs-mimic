# Object listing verification

Verified on **2026-10-22**, macOS arm64, Rust 1.99.1 / edition 2024 and the
committed Cargo graph. Runtime commits: `41c2490`, `536973a` and `eb3aa6f`.
The [slice contract](../slices/json-object-list.md) defines supported behavior;
[the ledger](../../../compatibility/features.json) owns implementation status.

## Results

| Command | Result |
| --- | --- |
| `cargo +1.99.1 test --locked --offline --test object_list_http --lib` | PASS: six new HTTP tests and 53 library tests |
| `cargo +1.99.1 test --locked --offline --test object_list_http --test bucket_http` | PASS: six listing and six bucket HTTP tests |
| `cargo +1.99.1 test --workspace --locked --offline` | Earlier PASS: 162 tests; latest runs fail the pre-existing catalog deadline case described below |
| `cargo +1.99.1 test --workspace --locked --offline -- --skip obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish` | PASS: 161 tests; one known unrelated case explicitly filtered, zero ignored |
| `cargo +1.99.1 fmt --all --check` | PASS, exit 0 |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS, exit 0 |
| `GCS_MIMIC_GO_TEST_RUN='^TestObjectList$' GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` | PASS once: TestObjectList and nine subtests; harness also runs Go vet |
| `cargo +1.99.1 xtask compatibility validate` | PASS: 591 feature rows; inventories remain separate |

The full-run failure, final scoped Rust and fmt/clippy output, and single Go run are retained in
[the check log](object-list-checks.txt). Machine-local path prefixes are replaced
with placeholders; other raw output is unchanged. Targeted runs guided the
implementation; the retained final run supersedes their results.
The existing `obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish`
failed in full and isolated runs with `HTTP response headers` EOF at
`tests/object_http.rs:144`; a later isolated rerun passed. The coordinator also
reproduced the same intermittent failure on unchanged main `56118ff` in two of
three runs and assigned its repair to a separate task. This slice does not change
or disable that test; the final verification command explicitly filters it, and
the unfiltered failure remains visible in the log. A fully green unfiltered final
suite is therefore not claimed.
Go SDK and guide execution used `536973a`. The later `eb3aa6f` defaults zero page
sizes and empty context filters, and strengthens HTTP assertions; final Rust
checks cover those changes. The coordinator explicitly waived another SDK run.
Markdownlint uses the cached CLI and disables only MD013 for API tables and long
commands; all other default rules remain enabled.

## Verified behavior

- Empty arrays and final continuation tokens are omitted; every item is exactly
  the existing metadata GET resource for its selected record. Full projection
  uses the existing modeled subset, including its ACL/owner limitation.
- Prefix, arbitrary complete delimiters, inclusive start/exclusive end offsets,
  empty/equal/reversed bounds, root-relative names, multibyte UTF-8 and a byte
  successor that is not itself valid UTF-8 pass wire tests.
- Combined maxResults defaults/clamps to 1000 for both projections. A 1001-object
  fixture verifies the default/cap and a single final object. Size-one pages
  include each prefix once and preserve both representations of a trailing marker.
- Invalid/mismatched/corrupted tokens and malformed scalar/query inputs fail
  explicitly. Missing bucket is 404 `notFound`; body rejection is 400
  `invalidArgument`; unsupported semantics are 501 `notImplemented`.
- Collection POST retains the old unimplemented-route 404; PUT/PATCH/DELETE
  return 405. Existing bucket route coverage remains unchanged and passes.
- The offset continuation starts with only `prefixes: ["g/"]` and no items;
  the next page has no repeated prefix after new children are inserted.
  Between pages, deleting the cursor and lookahead objects, inserting behind or
  ahead, overwriting, deleting a marker with surviving children and restarting
  do not duplicate returned entries or skip surviving names ahead of the cursor.
- A storage fixture has 20,000 children under one prefix. An actual SQLite
  progress callback interrupts after 10,000 VM instructions; listing that prefix
  and continuing to the next item finishes below the budget. This proves bounded
  seek work for the fixture, not RSS/CPU/latency or throughput performance.
- Go 1.27.3, Storage v1.69.0 and API v0.306.0 use the committed module graph.
  Bucket.Objects with nil/default full projection, PageInfo.MaxSize=1 and
  iterator.NewPager size 1 pass for prefix, delimiter, trailing markers, offsets,
  reverse bounds, multibyte names and a multi-character delimiter. Media uploads
  are raw HTTP fixture setup; SDK Writer is not claimed.

## Executed getting-started workflow

The [guide transcript](object-list-guide.txt) records the slice document's curl
commands against an isolated native server with an explicitly selected loopback
origin. It creates `list-demo`, verifies an empty list, uploads three five-byte
objects, lists `docs/` with `/`, requests a single-entry page, uses the actual
returned token and verifies an empty matching prefix. The server exits through
SIGTERM and its temporary catalog is removed.

Observed first-page names are `docs/` and `docs/a.txt`, with common prefix
`docs/nested/`. The size-one page returns `docs/` plus a token; continuation returns
`docs/a.txt` and `docs/nested/` without a further token. Empty results are exactly
`{"kind":"storage#objects"}`. Generation values, timestamps and the selected port
in the log are real run output rather than deterministic fixtures.

## Limits and ledger updates

JSON-OBJECTS-008 and BEHAVIOR-OBJECT-018 advance to PARTIAL; the listing evidence
extends existing PARTIAL BEHAVIOR-COMMON-009/013. New acceptance cases and exact
test/verification links describe the implemented live-list subset. No other
feature rows or generated matrices are changed.

SPEC-LIST-PAGING records zero-as-default, local checksum/query binding,
marker accounting and the absence of real-service differential evidence.
SPEC-LIST-RESOURCE-SHAPE records empty omission and conflicting slash-specific
documentation versus requested-delimiter behavior. Other pre-existing contract
gaps do not authorize broadening this slice.

Auth/IAM/ACL, full projection parity, nonempty glob/context filters, fields,
soft-deleted/history/folder listing and billing remain unsupported. Empty
matchGlob/filter and false feature flags are real no-feature defaults accepted
for the pinned SDK. Zero maxResults selects the local default; production behavior
remains unverified. Checksummed tokens are not cryptographically authenticated
snapshots.

No container build/smoke, full SDK suite, Google Cloud/metadata-server call,
real-cloud differential test, cross-platform run, memory/CPU benchmark, matrix
regeneration, push or release was performed. The coordinator owns integrated
container/full SDK validation and generated-view reconciliation after merging.
The next independent slice can implement a supported list extension or object
transfer feature without changing this keyset contract.
