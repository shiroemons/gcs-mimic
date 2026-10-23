# Object copy verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on 2026-10-23. The [declared contract](../slices/object-copy.md) owns JSON-OBJECTS-003 and BEHAVIOR-OBJECT-023. Both currently remain PARTIAL with EXACT fidelity for the frozen local subset while final integration is pending. The method additionally lacks ACL/owner resources and extra permission behavior for accepted full projection. Undocumented provider questions are tracked outside declared acceptance in the gap ledger.

## Source and implementation

Official documentation, Discovery revision/digests, protobuf pin and the Rust stable release check are recorded in the contract. The implementation is `edc59c420c14d6116efd16fbc8706dc17d08cff1`, correcting the initial copy commit `5784a40d7c6db6d334467f50762f62e4d0085f6c`. Shared transfer commits are `8d57890bc0072dade2ab9596e43fc70c5e35430f`, `8ed766f5237ecd0e54ca76159f8538821a1053f8`, `daf43a03b0cee8a69dab2450142ca14d54efa0e9` and `215e9148764493365ab4ec027b1ee8f8d2c488a8`. No dependency was added.

## Executed checks

| Command | Result |
| --- | --- |
| `cargo fmt --all --check` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo test --workspace --locked --offline` | PASS, 499 tests in the recorded full run, including nine copy wire cases and six deterministic copy workflow cases |
| `cargo test --lib --locked --offline shutdown_during_copy_recovers_without_a_destination` | PASS; additional deterministic shutdown/reopen regression |
| `cargo test --test resumable_lifecycle --locked --offline` | PASS, 19 existing lifecycle cases |
| `GCS_MIMIC_GO_TEST_RUN='^TestObjectCopy' bash scripts/sdk-go.sh` | PASS, generated JSON Copy and hermetic CopierFrom transport check |
| `cargo xtask dependencies check` | PASS at slice start; all existing pinned inputs current |
| `cargo xtask compatibility validate`, `render`, `check` | PASS; 591 rows and both generated matrices |
| `npx --yes markdownlint-cli2 README.md docs/en/roadmap.md docs/en/getting-started.md docs/en/getting-started-spec.md docs/en/slices/object-copy.md docs/en/evidence/object-copy.md` | PASS, zero issues |

The Rust socket tests ran with local listeners available. The [Rust output](object-copy-tests.txt), [Go output](object-copy-go.txt) and [dependency output](object-copy-dependencies.txt) retain one final evidence set with local checkout prefixes replaced.

## Proven local behavior

- Nine wire cases cover source generation selection, all source/destination conditions, source and destination names with encoded slashes/Unicode/literal percent escapes, same-object and cross-bucket copies, explicit-body replacement versus absent/null inheritance, distinct strict source/destination grants, event holds, versioning, MD5-only and CRC32C mismatches, projections/fields, invalid/unsupported/missing inputs, a 200,003-byte payload, empty objects, competing create-if-absent requests, composite count/class/location interactions and restart equality. Failure classes assert zero retained transfers and staging entries.
- Seven deterministic workflow cases pause actual multi-buffer disk transfer to cover slow streaming beyond the catalog wait budget, handler cancellation, injected advance failure, source replacement during streaming for different-name and same-object copy, a destination create after advisory validation, destination default-class changes and composite count at commit, queued-publication admission timeout without a later commit, and shutdown/reopen without a destination. Temporarily removing the request guard made the cleanup regression fail with `Elapsed(())`; restoring the guard passes. The recorded final source retains the guard.
- Storage regressions cover ordered ranges with persisted incremental MD5/CRC32C; source deletion/overwrite after pinning; acknowledged-prefix restart and tail truncation; rename-before-commit recovery; missing/corrupt prefix quarantine; same-name pre-publication validation; atomic callback deletion and rollback; post-commit blob reclamation and maintenance retry; isolated cleanup failures preserving results; non-owned ID protection; incomplete/transient retry; atomic request binding; expiry; cancellation while begin is blocked; dropped handler ownership; busy-transfer/resumable maintenance coexistence; and foreign-key binding teardown on cancel, expiry and quarantine.
- The Go harness pins Go 1.27.3, Storage v1.70.0 and generated API v0.308.0. Generated `Objects.Copy` copies bytes with source/destination conditions, nil-resource inheritance, metadata replacement and response selection. The high-level `ObjectHandle.CopierFrom` uses `objects.rewrite`, proven by a hermetic transport assertion; this is not an emulator rewrite-completion claim.

## Remaining boundaries

ACL/default ACL behavior, full owner/ACL projection permissions, encryption/KMS, requester billing, contexts, retention and temporary holds remain unsupported. Cross-location/class copy uses local disk transfer and modeled class/location metadata; Google's large-object single-rewrite timeout/413 thresholds, replication and placement are not simulated. Empty object body `{}` replacement versus no-body inheritance, literal-null compatibility, exact errors/precedence and provider defaults remain named local interpretations or source questions. Composite XML ETag parity belongs to the compose slice.

No real-cloud request, process RSS/cgroup benchmark, container build, full SDK suite or full native/Docker getting-started execution was performed for this slice. The integrated heavy gate runs once after copy/rewrite/compose merge and final-source approval; the guide examples are provided without claiming their execution. The method's full owner/ACL projection remains an explicit missing acceptance surface; provider body/default/error/placement questions are outside the frozen local acceptance scope.
