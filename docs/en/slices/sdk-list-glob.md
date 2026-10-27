# Object listing glob filters

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: BEHAVIOR-OBJECT-019. The row remains PARTIAL: interval-filter acceptance and object-context filters are outside this slice.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| MATCH-GLOB-SYNTAX | Match complete UTF-8 names using `*`, `**`, directory-boundary `**/`, `?`, character classes/ranges/negation, escapes and nested brace alternatives. Reject malformed patterns and patterns exceeding 1024 bytes with 400. Braces cannot contain slash or double-star. |
| MATCH-GLOB-LIST | Filter JSON objects.list items and delimiter prefixes independently; combine prefix, slash delimiter, versions, pageToken and maxResults without skipped matches. Empty matchGlob is absent. Reject incompatible delimiters. Bound memory by pattern/name/page sizes and release SQLite reads before matching. |
| MATCH-GLOB-SDK | Exercise Go storage.Query.MatchGlob and Ruby bucket.files(match_glob:) through ordinary high-level calls, including paged results. |

Offsets already work and retain their behavior; this slice does not add offset acceptance, object-context filtering, managed folders or XML glob filtering.

## Sources and local decisions

Checked 2026-10-27 against the [official objects.list specification](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/list), last updated 2026-10-22, and the pinned Storage v1 Discovery revision 20261020 (SHA-256 `eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc`).
The specification permits short/empty pages with continuation tokens but does not mandate exact internal scan boundaries. The emulator scans candidates until a matching page is full or exhausted; clients must always follow nextPageToken. Empty-pattern normalization and malformed-pattern error details are local choices recorded in SPEC-LIST-GLOB-EDGES, without real-cloud parity claims.

## Verification

Acceptance was recorded before implementation. On 2026-10-27, the unchanged server returned 501 notImplemented for Go Query.MatchGlob (the SDK retried until its deadline) and Ruby bucket.files(match_glob:) (UnimplementedError). Both ordinary calls pass after implementation.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `cargo test --locked --offline --lib match_glob`: PASS, two tests covering documented operators, Unicode, byte limits, deep nesting and arbitrary-pattern panic safety.
- `cargo test --locked --offline --test object_list_http`: PASS, all eight cases, including prefix/delimiter interactions, versions, size-one pages, changed query tokens and errors. The final added deletion-between-pages assertion also passes in the targeted two-case run: the continued empty result omits nextPageToken.
- `BUNDLE_FROZEN=true bundle check` with pinned Ruby/Bundler: PASS.
- `just check-sdk`: PASS, one final-source HTTP execution. Go vet and the complete Go suite pass, including four glob variants inside TestObjectList; Ruby self-check, migration and permissive/strict suites pass with ruby-match-glob in both profiles.
- The one final-source `just check` execution completed formatting, host/cross-target clippy and all 466 library tests successfully. Complete integration-gate verification remains assigned to the merged main branch; this slice does not claim a passing full `just check` run.
- `cargo xtask compatibility check`: PASS, including both generated feature matrices.
- `npx --yes markdownlint-cli2 docs/en/slices/sdk-list-glob.md docs/en/fake-gcs-server-migration.md`: PASS, zero issues.

The matcher uses a bounded NFA instead of expanding alternatives or backtracking. Each indexed candidate seek is finished before its name/prefix is matched, and the JSON listing path holds no transaction across matching. The existing serialized catalog worker preserves authorization/list consistency. No catalog schema or object mutation path changes.

The slice-start `cargo xtask dependencies check` reports current Rust 1.99.1 and all direct Rust dependencies; it exits 1 for pre-existing google.golang.org/api v0.309.0 and SBOM scanner digest updates. No dependency, toolchain or lockfile changes are made.

Container smoke and full guide journeys are outside scope and were not executed: Dockerfile and guide commands are unchanged, and existing smoke checks do not exercise glob listing. Benchmarks and real-cloud differential tests were not run. Empty-input normalization, exact malformed-pattern error details and scan boundaries retain the gap above. The preceding Ruby defaults slice has no Japanese slice companion, so this slice follows the same English-only contract layout; both generated feature matrices retain the same ledger status.
