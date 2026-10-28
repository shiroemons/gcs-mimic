# Seed source adapter

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: UX-002 and the seed subset of DESIGN-062, DESIGN-072 and DESIGN-073.
The source format and MIME inputs remain pinned by
[the source contract](source-storage-contract.md). No durable importer, startup
ordering or storage internals change.

## Acceptance cases

Defined before implementation. Results below refer to native macOS ARM64 with
the pinned Rust 1.99.1 toolchain, edition 2024 and the committed lockfile.

| Case | Required behavior | Result |
| --- | --- | --- |
| source-seed-profile | Literal names, empty buckets, binary/empty bodies, pinned extension MIME (now the default; octet-stream is opt-in) and Go suffix/case rules; skip/count regular OS metadata at every depth while traversing matching directories | PASS |
| source-seed-safety | Preserve native validation, read-only sources and strict authority while replaying fixtures on every startup; use bounded dedicated traversal and file publication; shutdown drains side effects and removes uncommitted staging | PASS |
| seed-content-type-config | CLI > environment > JSON > default; reject empty/unknown values; seed-only MIME leaves durable input metadata untouched | PASS |

## Replay update (2026-10-29)

The startup lifecycle in [the seed contract](seed.md#replay-on-every-startup)
replaced one-time import with replay on every startup. Each fixture file is
created when missing, left `unchanged` when the live object is exactly what
seeding would write, `replaced` with a new generation otherwise, or counted
`blocked` with a `seed_blocked` warning when a hold or retention rejects the
replacement. `seed_content_type` now defaults to `extension`; `octet-stream` is
opt-in. Top-level regular files are skipped with a `seed_skipped` warning
instead of failing startup. `default_seed_dir` supplies a seed root only when
`seed_dir` is unset and the path is an existing directory disjoint from
`data_dir` and `sync_dir`. The verification table below is the historical
record of the adapter slice; current replay results are recorded in the seed
contract.

## Boundaries

Replay skips OS metadata files, so API objects at such names are neither
replaced nor deleted. Matching
directories are traversed rather than silently dropping their children. No
generated upstream seed ACL is imported; stored durable ACLs remain unsupported.
The [release note](../releases/seed-adapter.md) explains the fresh-catalog change.
No repository dependency or benchmark harness is added.

## Implementation and verification

The adapter uses the shared Walker on one dedicated traversal thread with a
two-slot entry channel. Its existing 10000-entry and 32-directory-depth bounds
still apply, including skipped files. Objects are staged from the file and
published with `publish_objects` under an absent or exact-generation condition
and native metadata; a live object that already matches is detected by hashing
the fixture read-only before staging. No transaction spans traversal or body
staging. File staging
retains shared source-change and cleanup checks.

The fixed 1406-entry table is assembled from the pinned Go built-ins, mailcap
file-order overrides and YAML additions, then includes lowercase lookup aliases
and is sorted by extension bytes. Go normalizes text subtypes when adding
`charset=utf-8`, while non-text MIME values retain their original casing;
the fixtures cover both rules. Every key is ASCII except U+00B3; the only
non-ASCII Go Unicode 17.0 simple-lowercase mappings that can reach these keys
are U+0130 to `i` and U+212A to `k`. Tests assert the key alphabet and representative
Go suffix/case results, so host Unicode/MIME database versions do not affect lookup.
The [manifest](../../../compatibility/upstream/source-storage-contract.json)
records input and output digests. The Go BSD notice is retained in `src/mime.NOTICE`
and the existing archive/container notice aggregator includes it as `mime/NOTICE`.
Dockerfile and its copy rules are unchanged.

| Command | Result |
| --- | --- |
| `cargo xtask dependencies check` | PASS; all checked inputs CURRENT on 2026-10-25 |
| `cargo test --locked --offline --lib mime::tests` | PASS; one fixed-profile test |
| `cargo test -p xtask --locked --offline release::tests::notice_only_output_needs_neither_git_nor_an_executable` | PASS; archive/container notice aggregation |
| `RUST_TEST_THREADS=4 cargo test --locked --offline --test seed` | PASS; all 15 native tests, no skips or failures |
| `RUST_TEST_THREADS=4 just check` | PASS on integrated main `b181e2a`; fmt, clippy, workspace/doc tests, compatibility and shell checks |
| `just check-sdk` | PASS on final schema-17 source; pinned Go vet/tests and Ruby self-check, permissive/strict HTTP and V4 GET/PUT |
| `bash scripts/smoke-containers.sh linux/arm64` | PASS on final source; native image, default seed read-only mount, ready, stop/start/recreate, retained mutations/deletions and fresh-volume reseed |

The final source passes `mime::tests`, including SGML/DMClientScript normalization
and unchanged non-text casing, all 15 seed tests with schema-17 assertions, and
ledger render/check and formatting. Pinned Ruby 4.0.9, Bundler 4.0.23 and the
frozen lockfile are used with an isolated bundle path; no dependency files change.

The Shared state owner's [shutdown correction](shared-publication-shutdown.md)
is included. The existing seed signal test retains its one-committed-generation
assertion after rename, while cancellation during staging leaves no object or
temporary file; both paths release the lock and permit restart. The staging
observation accepts an already complete clone while its hashes are computed,
as well as a partially copied file. Native invalid-input, readiness, retry,
initialization and migration tests remain present.

Existing seed platform/container and migration evidence remains attached to UX-002;
it is not overwritten. The native ARM64 smoke covers the existing default seed
journey; extension MIME and OS metadata fixtures are verified on macOS ARM64.
Dockerfile and runnable guides are unchanged. Additional container architectures,
native Windows, real power loss and benchmarks are not run for this slice.
Durable filesystem publication, memory export and legacy reseed remain outside
this slice; DESIGN-062, DESIGN-072 and DESIGN-073 remain PARTIAL.
