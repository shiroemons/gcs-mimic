# Object metadata evidence

Verified on **2026-10-22**, native macOS ARM64, Rust 1.99.1. Runtime and Rust tests:
commits `5434d8f4f04f9c7dcc4eb471449da46033da4dfb` and
`074d39a93ea79a6a0d02b490a46c11cf1df39ca2` (final Rust source). Go SDK execution
used the first runtime commit. No real Google Cloud or metadata-server requests were made.

## Verified behavior

The [slice contract](../slices/json-object-metadata.md) defines the supported subset.
PATCH merges fixed/custom fields, null deletes custom keys, and PUT clears omitted
writable fields. Conditions are evaluated atomically; concurrent updates with one
metageneration condition have one 200 and one 412. Generation/body/hash/creation fields
remain unchanged, while metageneration, clock-derived updated and ETag transition.
Metadata survives restart; previous v3 objects migrate to v4 with defaults and the
existing v1 path still works. Existing blob crash/recovery tests remain passing.

HTTP tests cover matching and mismatching server-managed fields, full GET-to-PUT,
all four conditions for both verbs, live/nonlive generation selectors, fixed media
headers and nonpublic no-cache, malformed JSON/types, unsupported fields/queries, missing resources,
exact 8 KiB custom metadata and merged/UTF-8 overflow, 64 KiB request cap, null and empty
values, gzip rejection and no-transform media delivery.

## Commands and results

| Command | Result | Evidence |
| --- | --- | --- |
| `cargo +1.99.1 test --workspace --locked --offline` | PASS; 161 tests, zero failed/ignored | [Raw output](object-metadata-tests.txt) |
| `cargo +1.99.1 fmt --all --check` | PASS, exit 0 | Observed command completion |
| `cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS, exit 0 | Observed command completion |
| `GCS_MIMIC_GO_TEST_RUN=TestObjectMetadataUpdate GOPROXY=off GOFLAGS=-mod=readonly bash scripts/sdk-go.sh` | PASS once; Go vet and test | [SDK output](object-metadata-go.txt) |
| Getting-started snippet against an isolated native server | PASS; merge, key deletion, stale 412, replacement, body unchanged | [Output](object-metadata-guide.txt) |
| `cargo +1.99.1 xtask compatibility validate` | PASS; 591 rows, 401 parameter locations and 357 schema fields | Observed command completion |
| Markdownlint on both new Markdown files | PASS; only MD013 disabled for tables/URLs/commands | Observed command completion |

The Rust run includes 52 library, 6 bucket HTTP, 12 catalog, 11 foundation, 20 object
media HTTP, 4 metadata HTTP, 23 object storage, 20 xtask library, 11 compatibility and
2 dependency tests. Rust tests use only local sockets/files. A sandboxed attempt could
not bind localhost (`Operation not permitted (os error 1)`); the complete recorded run
used localhost permission and passed. During development a stale test fixture also
expected the old schema column count/version; those fixtures were updated rather than
removing assertions. The recorded set contains the final passing results.

One full run after the cache-header change failed the pre-existing
`obj_http_catalog_wait_deadlines_return_backend_error_and_commit_can_finish` test with
`HTTP response headers`, `left: 0`, `right: 0` (EOF). The unchanged source passed that
test alone, then passed the full run. No timeout or negative assertion was weakened.
Go was run before the subsequent cache-header and review fixes; the coordinator
explicitly deferred its rerun to the integrated suite.

Review checks add Content-Language length 100/101, explicit unsupported non-ASCII
headers/empty custom key, numeric read-only int64 equality, gzip q/wildcard negotiation,
real compressed bytes, upload/delete races, re-upload defaults, an original-schema v3
fixture and late migration failure rollback. Markdownlint uses the default rules except
MD013, disabled only for API tables, source URLs and copyable commands.

The Go harness pins Go 1.27.3, Storage v1.69.0 and API v0.306.0. It verifies Update,
merge, stale conditioned 412, Attrs, NewReader Content-Type/body, empty custom string
preservation and empty-map deletion. The SDK does not translate individual empty map
values to null; individual JSON key deletion is covered by HTTP instead.

## Limits

All affected ledger rows remain PARTIAL. Strict authorization, ACL/owner, retention,
holds, contexts, customTime, historical generations, other transports, gzip expansion,
double-compression detection and token response replay remain unsupported. Server-managed
input equality and rejection of unknown/different values is a conservative local rule;
legacy JSON service behavior still needs independent evidence. JSON media returns the
nonpublic no-cache directive; public ACL/IAM cache behavior remains unimplemented.
ETag encoding remains the existing local opaque representation.

No new container builds/smoke tests, full SDK suite, real-cloud differential tests,
Windows/native amd64 execution, power-loss tests or resource benchmarks were run.
Dependency freshness was supplied as already passed for this date; manifests and
lockfiles were unchanged. Generated ledger views are left to integration.
