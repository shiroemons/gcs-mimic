# Atomic object move

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: JSON-OBJECTS-009 and BEHAVIOR-OBJECT-028, same-bucket flat-namespace live objects only. No new configuration or catalog schema is required.

## Contract and acceptance

| Case | Required behavior |
| --- | --- |
| MOVE-BASIC | POST encoded source/moveTo/o/destination returns the destination Object; preserves bytes and modeled metadata, changes name/generation/create/update timestamps, resets metageneration to 1, removes the live source. |
| MOVE-ATOMIC | Source deletion, destination publication, conditions and authorization use one immediate catalog transaction; retain the opaque blob ID without reading/copying its bytes; rollback preserves both names. |
| MOVE-PRECONDITIONS | All four source and four destination conditions, including destination generation match 0, return 412 conditionNotMet on mismatch; mutually exclusive condition pairs return 400. |
| MOVE-ERRORS | Missing source returns 404; identical names return 400; held source/destination reject before mutation; strict source get/delete and destination create/delete checks are enforced in the transaction. |
| MOVE-VERSIONING | Moving the live source permanently removes that generation; older source versions remain. Replaced destination becomes noncurrent when versioning is enabled. |
| MOVE-SDK-GO | Default ObjectHandle.Move and conditional failure through ObjectHandle.If work with the pinned SDK. |
| MOVE-SDK-RUBY | Storage 1.62.1 Bucket#move_file uses objects.move; its default call passes in permissive and strict profiles. |
| MOVE-PENDING | HNS, folder rename, cross-bucket move, ACL/full projection permissions, retention/soft delete/Autoclass and provider-exact error precedence remain pending. |

## Sources and local choices

Checked 2026-10-27: [objects.move](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/move), updated 2026-10-22, specifies a same-bucket atomic operation, metadata inheritance, new generation/metageneration/timestamps, and destination versioning. The [objects overview](https://docs.cloud.google.com/storage/docs/json_api/v1/objects) still says HNS-only; SPEC-001 retains that source conflict. Flat-namespace support is the requested local scope, not proof of provider eligibility. Same-name rejection uses 400 invalidArgument because the method requires different names without specifying an exact error reason. Source live-generation hard deletion follows the method's source deletion rule; no real-cloud observation establishes its versioning details.

The Storage v1 Discovery revision 20261020 and SHA-256 eeb0831df501d131c9497534b2c3d130ea40f9fa215195115d011cb64e1a42bc pin the route and eight conditions, projection and userProject. Request bodies are forbidden. Unsupported billing and full ACL projection use the existing 400 badRequest response. No performance measurements are claimed.

## Verification

Implementation: `aae3703`. Both feature rows stay PARTIAL/EXACT for the tested subset, with the listed exclusions PENDING. The JSON adapter rejects `projection=full` and `userProject` through the existing fail-fast response rather than claiming ACL or billing support. Strict mode requires source get/delete and destination create, with delete for replacement; the alternative move-only permission is pending. Versioning retains a replaced destination through the existing publication helper. Source removal, publication, generation allocation and both journal entries commit together; no filesystem publication or source-blob deletion is needed. Normal reclamation handles replaced destination blobs after commit.

Verified on 2026-10-27 with Rust 1.99.1; no dependencies or lockfiles changed. `cargo xtask dependencies check` passed with every input CURRENT. The fresh official Discovery download has the revision and digest above.

- `cargo check --workspace --all-targets --locked --offline`: PASS.
- `just check`: PASS on final source, including formatting, clippy, Windows/Linux cross-target checks, all 1,023 Rust tests, the 591-row compatibility ledger/generated views, shell checks and release-script self-checks. No failed or ignored Rust tests.
- `cargo test --locked --offline --test object_move_http --lib move_`: PASS, six wire cases and three matching unit cases, including this slice's route and storage tests. `cargo test --locked --offline --test object_move_http concurrent_moves_create_if_absent_have_one_winner`: PASS, the seventh wire case omitted by that name filter. The SQL-trigger failure test proves rollback after source deletion; the restart case checks the exact blob ID and preserved bytes/metadata.
- `BUNDLE_FROZEN=true bundle check`: PASS with the pinned Ruby toolchain selected.
- `just check-sdk`: PASS, full Go suite plus Ruby self-check, migration scenario, permissive and strict journeys. Go 1.27.3 / Storage v1.70.0 / API v0.309.0; Ruby 4.0.9 / Bundler 4.0.23 / Storage 1.62.1 / generated Storage client 0.70.0. `TestObjectMove` and `ruby-object-move` use their default high-level calls; the Go conditional call returns 412. Before implementation both high-level calls failed with 400 badRequest.
- Markdown and Japanese textlint: PASS. Only the two stable feature-ID suffixes are allowed in the stock-boundary check, which otherwise mistakes them for ticket references; Japanese language/style rules remain enabled.

Container and complete guide verification are outside this slice; no completed container result is claimed. No benchmarks, resource measurements or real-cloud differential requests were run. Existing runnable guide commands were unchanged; the new Go/Ruby guide calls are exercised by the SDK tests. Move-specific SIGKILL/power-loss boundaries and provider-exact timing/error precedence were not separately exercised.
