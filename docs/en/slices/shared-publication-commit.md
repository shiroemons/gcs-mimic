# Shared publication commit

<!-- markdownlint-configure-file {"MD013": false} -->

This integration slice covers DESIGN-072 and DESIGN-074 without changing their PARTIAL status, the HTTP contract, schema 14, streaming, admission or filesystem durability policy.

## Acceptance cases

| Case | Required behavior |
| --- | --- |
| shared-publication-paths | Media, multipart, XML PUT, copy, rewrite, compose and resumable finalization share generation allocation, previous-generation retirement and object insertion inside the caller's transaction. Preserve metadata, component counts, checksums, authorization, conditions, holds and versioning. |
| shared-publication-rollback | Preparation/completion or SQL failure rolls back the generation counter, destination and deletion journal together. Reclamation runs only after commit and a reclaim failure does not fail committed publication. |
| shared-publication-recovery | Preserve derived rename-before-commit recovery, pinned source handles and hash checkpoints; preserve rewrite token progression and atomic completion replay. Existing process-kill recovery tests continue passing. |

## Transaction and recovery boundary

`objects::publish_in_transaction` borrows the caller's transaction and allocates `max(clock_microseconds, last_generation + 1)`. Its preparation callback receives that generation and returns the object plus any protocol result, keeping resumable response rendering before retirement/insertion. Protocol-specific preparation and completion stay inside that transaction: derived publication marks publishing, prepares metadata, completes rewrite/compose state and deletes transfer rows; resumable publication saves its rendered completion response. The caller commits before using `objects::reclaim_after_commit` for best-effort retired-blob reclamation. Derived source-link cleanup retains its position before retired-blob reclamation.

Derived recovery restores renamed, uncommitted output to staging and verifies the acknowledged prefix against saved hashes. A committed derived publication removes transfer/input rows atomically with its destination and rewrite completion response. Rewrite tokens, binding validation, previous-token replay, cached responses and deadlines remain owned by rewrite.rs and are unchanged. Ambiguous commit failures retain the existing reference probes and cleanup paths.

No new publication modes, batch hooks, hash helpers, filesystem helpers, journal schema or recovery scheduler are introduced. No design requirement or previously frozen decision changes.

## Verification

Acceptance cases were recorded before implementation. Code and the extended rollback test are committed as `40890667f1099a3905eff02954cf0648eb356a7e`. Verification uses macOS ARM64, Rust 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`), Cargo 1.99.1 and edition 2024 on 2026-10-25.

The existing `callback_source_deletion_rolls_back_with_destination_and_checks_holds` test now also checks unchanged allocator state, an empty pending-deletion journal and complete destination metadata after a failed completion callback. Existing derived recovery fixtures, rewrite token/replay tests, SQL-full retries and process-kill tests cover the shared boundary without adding production fault hooks.

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --test object_http --test object_multipart_http --test object_copy_http --test object_compose_http --test rewrite_http --test auth_http --test object_versioning_http --test object_hold_http` | PASS: all selected HTTP suites, including media/resumable/rewrite process-kill recovery |
| `cargo test --locked --offline --lib callback_source_deletion_rolls_back_with_destination_and_checks_holds` | PASS: strengthened transactional rollback assertions |
| `just check` | PASS on the final code: format, Clippy, full workspace tests (including 338 library tests), compatibility validation and offline shell/publication checks |
| `cargo xtask compatibility render` / `cargo xtask compatibility check` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields; both language views regenerated with no diff because displayed status/fidelity are unchanged |
| `cargo xtask dependencies check` | PASS: all checked inputs CURRENT, checked at 2026-10-25T02:05:40Z; no dependency or pin changes |
| `npx --yes markdownlint-cli2 docs/en/slices/shared-publication-commit.md` | PASS |

SDK, container-build and guide suites are excluded because no HTTP behavior, Dockerfile or guide changes. No benchmark or performance claim is made. Host power loss, every individual crash instruction boundary and native Windows execution remain unverified; broader publication modes, synchronization and import remain pending in the existing ledger cases.
