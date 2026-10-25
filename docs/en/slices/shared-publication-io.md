# Shared publication I/O

<!-- markdownlint-configure-file {"MD013": false} -->

This slice covers DESIGN-072 and DESIGN-074. Both remain PARTIAL: batch/file publication, synchronization, host power loss and broader recovery windows belong to later acceptance cases. No HTTP, schema, configuration or logging contract changes.

## Acceptance cases

| Case | Required behavior |
| --- | --- |
| publication-shard-durability | Flush a newly created shard's parent before publication; existing shards retain their established parent durability. Flush the body before rename and the shard after rename. Concurrent creation must not publish before the creating thread establishes parent durability. |
| publication-reclamation | Decide references in the serial catalog lane, then unlink retired immutable IDs and flush each affected shard outside that lane in bounded pages. Remove journal entries only after successful file durability; failure preserves committed success and retryable journal state. Concurrent reads/publications and shutdown retain ownership. |
| descriptor-file-operations | Descriptor-relative single-component names; immutable-source cloning/copying with bounded fallback and unchanged source offset; exact file identity including nanosecond ctime. Conditional absent-target rename never overwrites a concurrent entry. Exchange retains displaced bytes at the staged name, reports unexpected identity and never blindly exchanges back or unlinks conflicts. Unsupported capabilities fail explicitly. |
| publication-io-recovery | Existing media, derived and resumable kill/restart tests retain committed bytes and recover uncommitted rename output; failed reclamation retries at restart. |

Recovery flushes `blobs/` once before reconciling catalog state: a pre-crash shard creator may have stopped between creating its entry and syncing the parent. Runtime shard creation remains serialized through the parent flush, including concurrent publishers; body and shard flushes retain their ordering.

## Boundaries

Orphan scanning remains before bind. No concurrent orphan scanner or in-flight blob registry is needed in this slice; moving scanning after readiness remains conditional on the protection contract in [onboarding state](onboarding-state-contract.md). Reclamation only handles durably retired, immutable blob IDs which cannot be republished. The journal/reference decision remains serialized; file removal and directory synchronization do not hold SQLite execution.

One cleanup slot admits at most one owned task, retaining the catalog lock through cancellation and shutdown. Existing bounded request/upload owners wait for that slot before acknowledging publication, without holding SQLite execution; no independent work queue is created. Each invocation reads at most 128 IDs and groups directory flushes by shard; a larger backlog can leave another page for the next mutation, maintenance pass or startup. Successful removals clear both deletion journals in a short transaction only after file durability. ACTIVE/CANCELLED resumable sessions retain their markers for the session cleanup owner, which also removes staging; COMPLETE snapshots do not pin retired bodies. Error cleanup and startup recovery retain their existing reference checks. The negative permissions fixture keeps both shards read-only so retries cannot accidentally repair the intended failure before restart.

`clone_into(&File, &File, &OsStr)` attempts cloning only and returns `FsError::Unsupported` without creating an entry when no clone capability is available. This lets shared-publication-modes perform its own single-pass read/write/hash fallback for mutable inputs. `clone_or_copy_into` is for immutable open blobs only: clone, Linux `copy_file_range`, then 64 KiB positioned read/write; it returns `CopyMethod` and preserves the source offset. Files are flushed before successful return. Failed copies can leave a new entry and require caller cleanup. The directory descriptor plus single-component names prevent accidental path traversal; callers still own the directory and temporary-name namespace.

`stat_identity` records device, inode/file index, size and signed Unix nanosecond mtime/ctime (Windows has 100 ns granularity). `durable_replace` flushes the staged file, performs Unix exclusive rename or exchange, then flushes the directory. Exchange compares the pre-operation full identity and displaced inode/size/mtime; rename itself can update ctime. Displaced entries remain at the staged name for the caller's conflict policy even on success, including unexpected regular files, symlinks and directories. No exchange-back or unlink occurs. In-place writers and name ownership still require Sync's settle/conflict policy; this primitive does not freeze external writers. Errors after rename/directory flush are potentially applied and require reconciliation, never automatic cleanup.

Windows copies use bounded positioned read/write and new-target moves use `MoveFileExW(WRITE_THROUGH)`. Existing-target exchange explicitly returns Unsupported rather than overwriting bytes without preservation. Sync owns its separately documented checked-replace fallback and check-to-replace race. Directory handles need Windows backup semantics when opened by the caller. No adversarial-writer safety, Unix directory-fsync equivalence or host-power-loss namespace guarantee is claimed. No design decision or schema changes.

## Sources

Checked 2026-10-25; SHA-256 pins fetched HTML bytes. These OS contracts do not change Google API semantics or resolve a new compatibility gap.

| Source | SHA-256 |
| --- | --- |
| [Linux rename manual](https://man7.org/linux/man-pages/man2/rename.2.html) | `8772946a809a0db54f4854f200908f10bee758ea452e8df5efd955bc3b8e6e57` |
| [Apple APFS APIs](https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/APFS_Guide/ToolsandAPIs/ToolsandAPIs.html) | `188188a1cc71c5ac9e71c6cc048abf0748930d07d93a01addfa01418e49f59ab` |
| [Microsoft MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) | `36cc70da830043ee32e99f3789b8e5b78dfa0721cd9224b024fc59fa81f3f967` |

## Verification

Acceptance cases were recorded before implementation. Code is committed as `83f5b9d`, `9e691cc`, `0ef9c43` and `e5b9343`. Native verification uses macOS ARM64, Rust 1.99.1, edition 2024 and the unchanged locked dependency graph on 2026-10-25. The slice-start official freshness check reports every checked compiler, crate, SDK, action and container input CURRENT; no dependencies or pins changed.

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --lib fsutil` | PASS: six tests, including exclusive clone/copy names, zero/multiple-buffer bytes, unchanged source offset, bounded fallback, identity and displaced/conflicting/symlink preservation. |
| `cargo test --locked --offline --lib reclamation` | PASS: all six cases; bounded pages, live-reference refusal, cancelled cleanup/lock ownership, failed staging cleanup marker retention, publication wait/read continuity, parallel media/derived/resumable publication with preserved committed bodies after restart, and derived failure/maintenance retry. |
| `cargo test --locked --offline --lib publication_waits_for_reclamation_without_holding_catalog_execution` | PASS: successful publication waits for retired-byte cleanup, while the new generation remains readable through the catalog during that wait. |
| `cargo test --locked --offline --test object_compose_http` | PASS: all 18 cases, including atomic source hard deletion and exact retained-blob counts. |
| `cargo test --locked --offline --lib committed_mutations_succeed_when_reclaim_is_deferred_to_restart` | PASS: committed overwrite/delete survive unlink denial and recover at restart. |
| `cargo test --locked --offline --lib cancel_reloads_terminal_state_when_transition_no_longer_matches` | PASS: resumable cleanup keeps staging ownership. |
| `cargo test --locked --offline --lib failed_finalization_and_cancel_cleanup_retry_before_session_deadline` | PASS: session-owned cancelled markers survive the shared cleanup pass until staging is reclaimed. |
| `just check` | PASS on final code `e5b9343`: format, Clippy, all workspace tests (357 library cases), HTTP and process-kill recovery, compatibility and offline shell/publication checks. |
| `cargo xtask compatibility render` / `cargo xtask compatibility check` | PASS: 591 rows, 401 parameter locations and 357 schema fields; English/Japanese matrices regenerated from the ledger. |
| `cargo xtask dependencies check` | PASS: all checked inputs CURRENT. |

The sole small native HTTP run at source `9e691cc` published and overwrote 1,000 objects of 1 KiB each once: 17.963 s and 17.399 s, zero errors with checksum and increasing-generation assertions; it overlapped test activity and predates the final contention-wait fix, so it is not final-source or comparative performance evidence.

Windows fsutil-only metadata compilation using `rustc --target x86_64-pc-windows-gnu --emit metadata` and the locked windows-sys metadata passes. Whole-crate Windows cross-check is blocked by missing `x86_64-w64-mingw32-gcc` in ring/libsqlite3-sys; native Windows execution and native Linux execution remain unverified. SDK, container-build and guide suites were not run because no HTTP behavior, Dockerfile or guide changed. Host power loss, every individual fsync crash window, unsupported-filesystem runtime branches and future mutable-file/batch publication remain outside this slice's verified scope.
