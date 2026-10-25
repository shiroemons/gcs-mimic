# Shared blob identity and hashing

<!-- markdownlint-configure-file {"MD013": false} -->

This internal refactor preserves API behavior, schema 15, on-disk names and hash checkpoints. No new feature ID or implementation-status change is introduced. Existing DESIGN-072 `shared-publication-paths` and DESIGN-074 `shared-publication-rollback-recovery` cases remain the ledger coverage.

## Acceptance cases

| Case | Required behavior |
| --- | --- |
| Blob identity | Generate 16 random bytes, encode exactly 32 lowercase hexadecimal characters including leading zeroes, reject malformed persisted IDs, and construct `blobs/<first-two-hex>/<id>` and `staging/<id>` from validated IDs without fallible path construction. |
| Streaming hashes | Media, derived transfer, derived recovery and resumable finalization use the shared MD5/CRC32C hasher with their existing bounded buffers, expected-checksum errors and publication ordering. |
| Persisted checkpoints | Existing raw 88-byte MD5 state and separate CRC32C/offset columns round-trip unchanged; a transfer saved before the refactor can resume and recover afterward. The import checkpoint format remains unchanged. |
| Regression | Existing media, multipart, XML, copy, rewrite, compose, resumable, recovery and token tests pass without changed expectations. |
| Shared encoding | Bucket/object page-token payloads and V4 signing digests/signatures retain the same lowercase hexadecimal bytes; URL percent escapes remain uppercase. |

## Decisions and limits

`BlobId([u8; 16])` owns generation, canonical text parsing and paths. Existing catalog/string interfaces remain in place to avoid widening the refactor into publication/schema changes. String boundaries validate once before using the infallible typed path methods. Rewrite and upload-session tokens retain their existing lengths and use the shared lowercase encoder.

Resumable cleanup and prefix recovery validate persisted IDs before selecting filesystem paths. A session lease retains its validated `BlobId`, preserving infallible staging-path access without adding a panic. Malformed stored IDs fail before cleanup or recovery can select unrelated files. JSON page-token payloads and authentication digests/signatures share `encode_lower`; their existing checksums and uppercase URL percent encoding remain unchanged.

The shared hasher gains raw MD5 checkpoint conversion for the existing derived-transfer columns; storage does not adopt the importer's versioned checkpoint envelope. This preserves stored bytes and requires no schema migration. Resumable finalization continues reading its staged file once to verify hashes; eliminating that read is outside this slice. No frozen decision or design requirement changes.

## Verification

Acceptance cases were recorded before implementation. Code and tests are committed as `556292e7784b1bea04b8dfd9d7529b7c51d7f67c` and `065a5e34500b8ae922cc5f0c344653fb2d259ad7`. Verification uses macOS ARM64, Rust 1.99.1 (`58cf8f9c35b355199727627d07ae34a4e2846d67`), Cargo 1.99.1 and edition 2024 on 2026-10-25.

The new `canonical_ids_preserve_names_shards_and_leading_zeroes` test checks generated IDs, exact persisted names/shards, leading zeroes and malformed lengths, uppercase, non-hexadecimal, path and non-ASCII input. `derived_checkpoint_preserves_raw_md5_bytes_and_resumes` exercises old-format MD5 state at empty, partial-block, full-block and multi-buffer offsets, checks byte-for-byte state equality, resumes both digests and rejects wrong state lengths. Existing tests keep their assertions; the standalone rewrite-token harness also loads the shared hashing module used by its source.

`malformed_session_blob_ids_cannot_select_cleanup_or_recovery_paths` verifies that short, empty, traversal, uppercase and non-ASCII stored IDs fail before SQL/filesystem work while preserving an unrelated file.

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --lib hashing::tests` | PASS: 3 tests, including versioned import and raw derived checkpoints |
| `cargo test --locked --offline --lib storage::blob_id::tests` | PASS: 1 test |
| `cargo test --locked --offline --lib storage::catalog::derived::tests` | PASS: 22 existing tests, including checkpoint restart, same-length corruption and rename recovery |
| `cargo test --locked --offline --test rewrite_token` | PASS: 5 tests, including the unchanged token binding/tampering tests |
| `cargo test --locked --offline --lib storage::catalog::resumable_maintenance::tests` | PASS: 1 malformed-ID cleanup/recovery test |
| `cargo test --locked --offline --lib json::page_token::tests` | PASS: 2 existing token tests |
| `cargo test --locked --offline --lib auth::signature::tests` | PASS: 9 existing signing/canonicalization tests |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `RUST_TEST_THREADS=4 just check` | PASS on the final source: format, Clippy, full workspace tests (including 362 library tests and existing HTTP/process-kill recovery tests), compatibility validation and offline shell/publication checks |
| `cargo xtask dependencies check` | PASS: all checked inputs CURRENT at Unix timestamp 1792901271; no dependencies or pins changed |
| `cargo xtask compatibility render` / `cargo xtask compatibility check` | PASS: 591 feature rows, 401 parameter locations and 357 schema fields; both language views regenerated with no diff |
| `npx --yes markdownlint-cli2 docs/en/slices/shared-blob-id.md` | PASS |

DESIGN-072 and DESIGN-074 gain passing regression evidence for their existing shared publication and rollback/recovery cases; their PARTIAL status, pending cases and achieved fidelity are unchanged. No new feature or acceptance case is claimed.

A default-parallel full run returned 503 instead of 200 in `final_hash_scan_uses_request_budget_instead_of_catalog_wait_budget`; the unchanged test passed in isolation. The final full run uses four test threads without skipping tests or changing assertions/timeouts. The cause of the default-parallel failure remains unconfirmed.

No benchmarks, SDK runs, container builds or guide journeys are performed because this slice changes no wire behavior, Dockerfile or guide. Native Windows execution and host power-loss behavior remain unverified; existing platform durability limits are unchanged. Subsequent publication-mode, imported-identity and import-journal work remains owned by its separate slices.
