# Seed startup verification

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Contract: [seed then ready startup](../slices/seed.md). Date: **2026-10-22**.
Implementation: `8ad9e4ecdf8a034442a1291b9272df0d7d005cc4`.
Verified natively on macOS ARM64 with pinned Rust 1.99.1, edition 2024 and the
committed Cargo.lock. No dependency was added. No real-cloud bucket or Google
credentials were used. No metadata-server request, container build or container
run was performed.

## Final commands and results

| Command | Result | Evidence |
| --- | --- | --- |
| `cargo test --locked --offline --test seed` | PASS: 11 tests, zero failures | [Native seed tests](seed-tests.txt) |
| `just check` | PASS: fmt, clippy with warnings denied, 234 workspace tests, compatibility and offline shell checks | [Selected final output](seed-check.txt) |
| `docker compose -f compose.yaml -f compose.seed.yaml config --quiet` | PASS: exit 0 | Configuration validation only; no containers |
| `docker compose -f compose.yaml -f compose.seed.yaml -f compose.strict.yaml config --quiet` | PASS: exit 0 | Configuration validation only; no containers |
| `bash -n scripts/smoke-containers.sh` | PASS: exit 0 | Also part of `just check` |
| `git diff --check` and repository local-path scan | PASS: no whitespace errors or path matches before implementation commit | No repository-local machine paths introduced |
| Slice-start `just check-dependencies` | UPDATE AVAILABLE: exit 1 for Go Storage v1.70.0 versus v1.69.0 | [Actual freshness output](seed-dependencies.txt) |

The dependency checker found the Rust toolchain, all Rust dependencies and other
checked inputs current. The Go SDK update was reported for separate integration
work; it does not change this slice's dependency graph. No "all inputs current"
claim is made for this implementation commit.

## Observed behavior

HTTP tests verify bucket/object listing, empty buckets, nested and Unicode names,
binary/multi-chunk/empty bodies, exact bytes, size, MD5, CRC32C and octet-stream
Content-Type. Both permissive and strict startup import fixtures; strict reads
use the local admin fixture token. A custom seed project selects the owning
project without contacting Google.

Restart tests preserve overwrite bytes, a new object and patched metadata. A
missing fixture path is ignored after initialization. Deleting all objects and
buckets still does not replay fixtures; a separate fresh data directory seeds.
No-seed startup permanently initializes the catalog. Populated v4 migration
preserves user bytes and skips seeding; v1/v3 migration regressions also pass.

An invalid later bucket interrupts an import after a valid object commits. The
process exits without a ready event and the catalog reopens immediately. A retry
with a changed fixture body retains the original object generation/bytes and
imports the missing object after the invalid entry is fixed. A changed owning
project fails rather than adopting the old bucket.

The readiness test waits for the actual post-recovery seeding event while an
external SQLite transaction blocks publication. Catalog ownership is retained
and the listener cannot be reached until the transaction is released and import
finishes. Health/readiness then succeed with the imported object count.

Negative/boundary cases cover invalid names, missing/overlapping roots, top-level
files, symlinks, Unix sockets, 0-byte/exact-cap/oversize objects, 32 versus 33
nested directories, and more than 10000 filesystem entries. Directory and body
limits are implementation bounds, not measured RSS or performance results.

## Pending integration and limitations

The independent seeded section in `scripts/smoke-containers.sh` checks the
read-only mount, Compose wait, changed bytes across stop/start and recreate,
all-bucket deletion, and deliberate new-volume reseeding. It is **not executed**
in this evidence; the integration task owns the final container run.

`non_utf8_fixture_names_fail_closed` is Linux-only because the native macOS
filesystem rejects creating the invalid-byte fixture. Its Linux execution and
native Windows/Linux verification remain pending. No real-cloud differential,
SDK, memory benchmark, power-loss or individual seed fsync/marker crash-window
test was run. The source must remain stable and read-only during startup.

The ledger proposal keeps UX-002 PARTIAL until the pending platform/container
cases pass. Shared ledger, matrix and runnable-guide files were left to the
integration task; this evidence does not itself change their status.
