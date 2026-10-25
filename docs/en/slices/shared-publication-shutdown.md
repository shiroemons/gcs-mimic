# Accepted publication during shutdown

Staged publication already owns publication-request, batch and staged-handle
permits. Catalog close drains those permits before releasing SQLite or directory
ownership. After blob rename, shutdown must therefore allow the accepted batch
to acquire SQLite execution and settle its transaction. A new-admission health
check at that point incorrectly turned accepted work into `Closed`.

Execution acquisition retains the existing absolute deadline and typed Busy or
Closed failures, while admission for new work still closes normally. Both a
waiting caller and a cancelled caller retain owned publication work until its
commit and temporary-file cleanup finish. This does not bypass conditions,
checksums, holds or cleanup on genuine publication failure.

On 2026-10-25, `cargo test --locked --offline --lib
storage::catalog::admin::publication::tests` passed all 15 tests on macOS ARM64.
`publication_cancelled_renamed_batch_cleans_before_shutdown_unlocks` now checks
both caller states while execution is held: close retains directory ownership,
the accepted generation commits, staging is empty and restart reads exact bytes.
Format and all-target workspace clippy passed. The integrating seed adapter also
passes its existing process shutdown test, retaining the committed generation
after rename, cleaning uncommitted staging and permitting restart; see
[adapter verification](import-seed-adapter.md).
