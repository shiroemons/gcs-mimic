# Live synchronization CLI

<!-- markdownlint-configure-file {"MD013": false} -->

This source-checkout CLI extends the control subset of DESIGN-072 and
DESIGN-074. It connects to the running server and never opens `data_dir`, reads
server configuration, repairs a stopped catalog or grants administrative power.
The server's [current control contract](external-deletion-controls.md) owns
authorization, fixed revisions and mutation semantics. Parent statuses remain
PARTIAL; the native CLI/server operator journeys below pass, while exhaustive
fault, platform and parent-feature acceptance remain separate gates.

```sh
gcs-mimic sync status --endpoint https://127.0.0.1:4443
gcs-mimic sync status --view findings --endpoint https://127.0.0.1:4443 --revision FINDINGS_REVISION
gcs-mimic sync conflicts --endpoint https://127.0.0.1:4443 --limit 256
gcs-mimic sync status --view deletions --endpoint https://127.0.0.1:4443 --revision DELETION_REVISION
gcs-mimic sync rescan --endpoint https://127.0.0.1:4443
gcs-mimic sync approve --endpoint https://127.0.0.1:4443 --deletion-set DELETION_REVISION
gcs-mimic sync wait --endpoint https://127.0.0.1:4443 --timeout 10s
gcs-mimic sync resolve --endpoint https://127.0.0.1:4443 --conflict CONFLICT_ID --revision TARGET_REVISION --action restore --expected-generation 123 --operation-id restore-one
```

The uppercase values above are placeholders: copy the matching exact revision
from status. Findings revisions contain 48 lowercase hex characters and bind
the current observation/session; deletion revisions contain 32. Inspect every
fixed deletion page before approval. Supply `--cursor` exactly as returned and
`--limit 1..256`; the client encodes the cursor once and prints only one page.
A changed revision or stale approval fails; the CLI never substitutes a fresh
revision or automatically retries a mutation.

Conflict listing starts without a revision. Later pages accept the returned
32-character listing revision and opaque cursor; each item has its own target
revision, which is distinct from the listing revision. `resolve` requires an
explicit target, action and operation ID; flags map directly to the server's
bounded JSON request. Restore publishes through the shared API-origin journal so
the working tree receives the restored bytes. Ordinary external ingestion keeps
its separate echo suppression.

A conflict request names `conflict_id`, its inspected `revision`, `action`
(`restore` or `discard`), `expected_generation` (zero means absent), and an
explicit `operation_id`. A retained recovery request instead names
`recovery_id`, `revision`, `target_identity`, `action: "discard"`,
`confirmation: "backup_verified"` or `"discard_without_backup"`, and
`operation_id`. Do not mix target types. Preserve backups before choosing
discard. The conflict CLI uses `--conflict ID --revision REV --action
restore|discard --expected-generation N --operation-id ID`. Recovery uses
`--recovery ID --revision REV --target-identity ID --action discard
--operation-id ID` plus exactly one `--confirm-backup-verified ID` or
`--confirm-discard-without-backup ID`, whose value must match the recovery ID.
Mixed targets, missing generations, ambiguous confirmations and mismatched IDs
fail before a request. Reusing an operation ID with changed input fails; a missing response
does not prove that an accepted operation rolled back.

`--endpoint` requires a bare HTTP(S) origin with an explicit nonzero port.
All resolved addresses must be loopback, RFC1918 IPv4 or IPv6 unique-local;
public, metadata/link-local, unspecified and multicast addresses are rejected.
At most eight resolved addresses are accepted, and the checked numeric address
is the connection target. No ambient proxy, credential discovery, redirect or
Google endpoint fallback is used.

HTTPS verifies the embedded development CA by default. `--ca-cert PATH`
replaces it with the supplied CA while retaining hostname verification. That
option is invalid for HTTP; there is no insecure TLS bypass.

Use the CLI-only `GCS_MIMIC_SYNC_TOKEN` environment variable or `--token-file
PATH` (which takes precedence). A file holds one bearer token of at most 8192
bytes, with optional final newlines. Do not put this CLI-only setting in the
server environment. The server repeats its current control authorization;
Google bucket IAM or fixture-administrator authority does not bypass it.

Requests have a default 10000 ms total network deadline, configurable from
1 through 60000 with `--timeout-ms`, or `--timeout` with an integer `ms`/`s`
suffix. For `wait` this value is sent as the server's
observation budget; the client allows an additional one second to receive the
result. Waiting captures the server's fixed scan/journal fence; unrelated later
writes do not extend it indefinitely. A 408 deadline or 409 stopped result is
printed as JSON and returns nonzero. A timeout/disconnect does not cancel an
already accepted scan or publication; inspect status before retrying.

Headers are limited to 64 and 16 KiB, and both chunked and length-framed JSON
responses have a 256 KiB body cap. Malformed, truncated and oversized responses
fail. Non-success HTTP statuses print their bounded JSON and fail, including
explicitly unsupported operations. Conflict resolution remains subject to the
server's exact revision, identity, authorization and confirmation checks.

## Native operator acceptance

The native suite executes the actual CLI binary against actual loopback server
processes. Its existing four cases retain permissive rescan/wait/paging and stale
409s, disabled-sync 404, strict missing/invalid/insufficient credentials,
token-file precedence and a controlled 503 receiver without automatic retry.
The five additional cases cover these operator journeys:

| Test ID | Verified behavior |
| --- | --- |
| `live_cli_approves_every_frozen_deletion_page_and_rejects_replay_after_restart` | Removing 100 tracked files triggers the guard. Three CLI pages contain exactly the fixed set, denominator, scan revision and journal fence; API changes invalidate old pages/approval. Exact approval, finite wait and API 404s succeed; wrong, replayed and restart-stale approvals fail 409. The bucket and empty directory survive. |
| `live_cli_restores_and_discards_retained_conflicts_once_across_restart` | Inspect two bounded conflict pages, restore binary bytes and discard a zero-byte conflict. Stale generation and held-object restore fail without changing the winner or retained body. Successful restore reaches API and tree, and operation replay returns the same generation through restart; changed payload with the same operation ID fails. |
| `live_cli_requires_retained_original_owner_after_bucket_recreation_and_restart` | Strict controls require the original owner's explicit project-admin grant after its last bucket is deleted and the name is recreated under another project. Missing that grant denies reads, waits and resolution; full authority still cannot restore into another incarnation. Explicit discard releases historical read authority, while completed-operation replay still requires its original owner. |
| `live_cli_inspects_stopped_sparse_recovery_and_requires_exact_confirmed_discard` | A retained 1 TiB + 1 byte sparse workspace stops intake and remains inspectable and charged. Restart preserves its length and binary prefix. Stale revision, wrong target/identity, missing original-owner authority and insufficient confirmations retain the body. Exact confirmed discard completes once, then rescan/wait and restart preserve API metadata/bytes and remove only that workspace entry. |
| `live_cli_lost_resolution_response_does_not_retry_a_committed_restore` | A loopback relay forwards resolution to the real server, observes committed success, then drops the response. The CLI fails without a second connection; explicit identical replay returns the recorded generation without another publication. |

Registered conflicts are prepared using the public catalog publication and sync
reconciliation paths before starting the server. The sparse recovery case instead
constructs one Armed intent in a stopped test catalog using the existing schema.
The real server then runs its normal restart inspection, protection, quota and
authorization checks. This fixture models
`restart_protects_actual_armed_and_durable_bodies_without_quota_eviction` and
`recovery_sparse_over_limit_requires_exact_identity_and_confirmation`; it proves
operator handling of the resulting state, not the I/O failure that produced it.
Sparse inspection compares length and prefix without reading a terabyte body.
The confirmation is an operator attestation; the CLI does not verify a backup.

The existing [paired conflict/recovery snapshots](sync-conflict-recovery.md)
and [native write-back recovery](native-writeback-recovery.md) remain the
seam-fault and PowerLoss model evidence. No new durable write path is added by
the journal-origin correction. These snapshots do not establish physical host
power loss or every-instruction crash coverage.

Run the scoped checks with:

```sh
cargo check --tests --locked --offline
cargo test --locked --offline --test sync_cli -- --nocapture
cargo test --locked --offline --bin gcs-mimic sync_cli
cargo test --locked --offline --lib storage::catalog::sync_state::conflicts -- --nocapture
cargo test --locked --offline --lib storage::catalog::sync_state::writeback::recovery -- --nocapture
cargo xtask lint
cargo clippy --test sync_cli --bin gcs-mimic --locked --offline -- -D warnings
```

On 2026-10-31, macOS ARM64 with Rust 1.99.1, `cargo check --tests --locked
--offline` passed before tests. The final native target passed all nine cases
(zero failures or ignores), the CLI parser/mock target passed ten, conflict
storage passed 22 including fault/PowerLoss cases, and write-back restart
recovery passed 12 including paired snapshots. Production lint reported zero
violations; the selected CLI/binary Clippy gate passed with warnings denied.
The focused rustfmt, whitespace and two-file Markdown checks also passed.
The 100-object fixture uses an explicit 60-second CLI deadline for its initial
durable scan; subsequent waits retain a finite ten-second budget. This does not
change the CLI's ten-second default or establish a scan-latency guarantee.

The real-server admission/503 timing path remains separate from the controlled
receiver. Linux/Windows parity, exhaustive watcher/alias/overflow/bind-mount and
shutdown/crash boundaries, full parent acceptance and real Google behavior are
not established by these CLI cases. No SDK suite, container build or benchmark
is part of this slice. See the
[combined verification](../evidence/migration-integration.md).

The Google [preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions)
and [object holds](https://docs.cloud.google.com/storage/docs/object-holds)
references were checked on 2026-10-31; the fixed Discovery/protobuf baseline is
unchanged from [directory synchronization](directory-synchronization.md#sources-and-verification-boundary).
These are emulator-local controls. The comparison reference remains
fake-gcs-server v1.56.1, revision `896ece51e49eed0946089f3089f6343855c0d106`.
No dependency or toolchain changes are introduced. Official release pages on
2026-10-31 confirm the used Rust 1.99.1, Tokio 1.54.1 and serde_json 1.0.154 pins:
[Rust](https://blog.rust-lang.org/releases/),
[Tokio](https://github.com/tokio-rs/tokio/releases/tag/tokio-1.54.1),
[serde_json](https://github.com/serde-rs/json/releases/tag/v1.0.154).
