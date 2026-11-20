# Live synchronization CLI

<!-- markdownlint-configure-file {"MD013": false} -->

Since 0.8.0, this CLI extends the control subset of DESIGN-072 and
DESIGN-074. It connects to the running server and never opens `data_dir`, repairs a stopped
catalog or grants administrative power; it reads a server config file only to find the server
when no endpoint is given.
The server's [current control contract](external-deletion-controls.md) owns
authorization, fixed revisions and mutation semantics. Parent statuses remain
PARTIAL; the native CLI/server operator journeys below pass, while exhaustive
fault, platform and parent-feature acceptance remain separate gates.

```sh
gcs-mimic sync wait   # the server that GCS_MIMIC_LISTEN and GCS_MIMIC_SCHEME configure
gcs-mimic sync status --endpoint https://127.0.0.1:4443
gcs-mimic sync status --view findings --endpoint https://127.0.0.1:4443 --revision FINDINGS_REVISION
gcs-mimic sync conflicts --endpoint https://127.0.0.1:4443 --limit 256
gcs-mimic sync status --view deletions --endpoint https://127.0.0.1:4443 --revision DELETION_REVISION
gcs-mimic sync rescan --endpoint https://127.0.0.1:4443
gcs-mimic sync reject --endpoint https://127.0.0.1:4443 --deletion-set DELETION_REVISION
gcs-mimic sync reject && gcs-mimic sync wait   # the set the status reports as waiting
gcs-mimic sync approve --endpoint https://127.0.0.1:4443 --deletion-set DELETION_REVISION
gcs-mimic sync wait --endpoint https://127.0.0.1:4443
gcs-mimic sync wait --endpoint https://127.0.0.1:4443 --timeout 30m --require-no-findings
gcs-mimic sync resolve --endpoint https://127.0.0.1:4443 --conflict CONFLICT_ID --revision TARGET_REVISION --action restore --expected-generation 123 --operation-id restore-one
```

The uppercase values above are placeholders: copy the matching exact revision
from status. A view given without `--revision` reads the status first and lists
the page at the revision it reports, `findings_revision` or
`pending_deletion_revision`; the latter is absent while no deletion set awaits
approval, which the command reports. When that revision changes before the page
is read, it reads the status once more. Findings revisions contain 48 lowercase hex characters and bind
the current observation/session; deletion revisions contain 32. Inspect every
fixed deletion page before approval. Supply `--cursor` exactly as returned, with
the `--revision` of the page that returned it, and
`--limit 1..256`; the client encodes the cursor once and prints only one page.
A changed revision or stale approval fails; the CLI never substitutes a fresh
revision for one given or automatically retries a mutation. An accepted
approval says on standard error how many objects the server deletes, that their
buckets stay and that synchronization cannot bring the objects back, and names
`gcs-mimic sync wait`, which answers once the deletions are done. A decision on
a set approved already answers 409 saying its objects are deleted or being
deleted; one on a set rejected already says its files are written back, and to
delete them again after the write-back if they should go after all.

`reject` is the opposite of approval and takes the same `--deletion-set`
revision. It closes the set, so nothing approves or runs it, and writes each
member's file back from its object through the ordinary write-back, recreating a
missing bucket directory; no object is deleted, and the next inventory finds
nothing to delete. A file that already sits at a member's path stays as it is.
The accepted answer names the `decision`, and the command says on standard
error how many files the server writes back. `gcs-mimic sync wait` answers
once every file is back, and the status counts the files left as
`observation.rejected_writeback`; a write-back stopped part way, by a stop or a
failure, resumes at the next start, retry or rescan before deletions are looked
for again.
A stale or already used revision fails with 409, as approval does.

Without `--deletion-set`, `reject` reads the status and rejects the set it
reports as waiting for a decision, saying `Rejecting deletion set REVISION (N
files)` on standard error, so that a script can run
`gcs-mimic sync reject && gcs-mimic sync wait`. It sends that revision, which
the server matches against the set that waits then, so a set replaced in
between is refused with 409 rather than rejected unseen; with no set waiting it
fails without sending anything. A rejection deletes nothing, which is why it
may take the set unseen. `approve` always takes the inspected revision: without
`--deletion-set` it fails, naming the set that waits, its files and the
deletions view that lists it.

The command and the server must come from the same release. Since 0.20.0 the
command sends its version in `x-gcs-mimic-version` and the server answers every
control route with its own. A server refuses a request that reports another
version, or none, with 400 `versionMismatch` before authentication or any
catalog wait. The command refuses an answer from another version, or one
without a version, after printing it, and names the release to use. A server
before 0.20.0 sends no version, so it has already handled the request when the
command refuses its answer: inspect its status before retrying a mutation.

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

Without `--endpoint`, the command controls the server that the configuration
gives, found by the same code as `gcs-mimic healthcheck` finds it:
`GCS_MIMIC_LISTEN` and `GCS_MIMIC_SCHEME` (with `GCS_MIMIC_LISTEN_HTTP` under
`both`) and the config file that `GCS_MIMIC_CONFIG` selects, plain HTTP
wherever a listener accepts it, and a wildcard listen address reached on
loopback. When no request reaches a server there, because nothing listens or
what answers speaks neither its TLS nor its HTTP, it tries once more at the
server that the options of a gcs-mimic server running as the container's PID 1
give, such as `docker run IMAGE --scheme https`, which the environment of
`docker exec` does not see. A request that may have reached a server, such as
one that timed out, is not sent again. The command asks for `--endpoint` while
the configuration has errors or gives an origin it cannot reach. An explicit
`--endpoint` always wins. The commands its own advice names then leave
`--endpoint` out, since they find the server the same way. The server's answers
keep the `--endpoint URL` placeholder, which the command replaces with the
options that reached the server; its log events leave the placeholder out,
since nothing fills it there and a command run beside the server finds it the
same way, or asks for `--endpoint` when it cannot.

A request that gets no answer the command can read fails with the URL it
tried, what went wrong and its cause (the name did not resolve, the connection
or the TLS handshake failed, the HTTP exchange failed, the answer was too large
or not JSON, or no answer came in time), and the setting to check: `--endpoint`,
or the configuration when that gave the server. A timeout or a broken HTTP
exchange adds that an accepted control may still be running.

`--endpoint` requires a bare HTTP(S) origin with an explicit nonzero port.
All resolved addresses must be loopback, RFC1918 IPv4 or IPv6 unique-local;
public, metadata/link-local, unspecified and multicast addresses are rejected.
At most eight resolved addresses are accepted, and the checked numeric address
is the connection target. No ambient proxy, credential discovery, redirect or
Google endpoint fallback is used.
Private-network HTTP remains available for container networks; use HTTPS outside
loopback to protect bearer tokens in transit.

While deletion candidates are being recomputed, status retains their last complete
membership while refreshing current safety findings and revisions. Every read
rechecks authority and the journal fence; revision-bound
pages and approval reject an invalidated revision with 409. Approval and
rejection first inventory again a set that waits for them, at the current
journal fence, so neither a native notification nor an API write that left its
members as they were makes the revision the status named stale. An API write
that replaced or deleted a member's object, or updated a member's bucket, does:
the decision answers 409 and names the set that waits instead. The pages of the
set the status names stay listed until then, a bucket update included, since
listing deletes nothing; a page of a revision the status no longer names answers
409 with what became of it and the view of the set that waits instead. A refused decision
says what became of the set: approved or rejected already; not the set that
waits now, which it names and counts without saying why, since a replaced, an
earlier, a mistyped and another server's revision look the same; still waiting
after an API write raced the decision, which may be sent again; or no set
waiting. A refused rejection adds the rejection of the set that waits, which
deletes nothing. A refused approval names only the view of that set and asks
to approve its revision after checking it, never a command that approves it.
During native quiet settling, wait and rescan remain queued inside the actor.
A wait request observes its original deadline; if quiet has not been established
at that deadline it reports SourceChanging with HTTP 409, and the CLI asks again
while its own `--timeout` leaves time. Rescan runs after quiet or the
ten-second quiet cap instead of returning an unperformed scan as success.
Reaching that cap alone never completes wait; another quiet interval runs while
its original deadline remains fixed. Queued controls survive observer recovery.
Approval and resolution remain available while settling unless the bounded control
queue is full; a saturated queue returns busy.
Automatic deletion history is durable across restart and guarded rescans;
approval, a new complete baseline, or a complete zero-candidate observation after
established quiet resets it. Automatic and explicit scans use the same reset
condition. The native queue is checked before and after the scan, and its event
generation must remain unchanged with no pending event. A scan at the quiet cap,
with remaining candidates, or spanning new notifications cannot reset history.
Reset failures retain history and log their cause without discarding a successful
wait or rescan response.
The quiet interval has a ten-second cap, so a longer operation may have committed
deletions below the cumulative threshold before approval becomes required.

HTTPS verifies the embedded development CA by default. `--ca-cert PATH`
replaces it with the supplied CA while retaining hostname verification. That
option is invalid for HTTP; there is no insecure TLS bypass.

Use the CLI-only `GCSM_SYNC_TOKEN` environment variable or `--token-file
PATH` (which takes precedence). A file holds one bearer token of at most 8192
bytes, with optional final newlines. The server ignores this CLI-only variable.
The old `GCS_MIMIC_SYNC_TOKEN` always fails with replacement advice, including
an empty value or a simultaneous new token or token file. The server repeats its current control authorization;
Google bucket IAM or fixture-administrator authority does not bypass it.

Rescan has a default 30000 ms total network deadline to allow for the ten-second
quiet cap followed by scanning. Other commands except `wait` default to 10000 ms,
configurable from 1 through 60000 with `--timeout-ms`, or `--timeout` with an
integer `ms`, `s` or `m` suffix.

`wait` defaults to two minutes and takes up to 60 minutes (`--timeout-ms` up to
3600000). The default covers the changes of a test step once the server is
healthy: the healthcheck fails until the first pass is done and allows it a
5-minute start period, because a first pass over a local-development tree of
about a thousand objects can take several minutes on a slow bind mount. A wait
that may itself cover a first pass needs a longer `--timeout`. A server observes one request for at most a minute, so the command sends
requests of at most a minute each, carrying the time left as the server's
observation budget and allowing one more second to receive the result. It asks
again only while the answer says the work is still in progress: `Timeout` for
that request, `SourceChanging`, or `JournalChanged` after a one-second pause.
Each repeat is an `info` diagnostic on standard error, and standard output keeps
only the last JSON answer. A pause, findings or conflicts that need action,
deletions awaiting approval, source entries no object can take or that the
server cannot read, such as a file without read permission, and stopped work
end it with the first answer. A wait that runs out of time names what was still
in progress: how many files were still changing, and the first of them.

A wait succeeds once every change it can reflect is reflected. Objects whose
names the directory cannot hold stay reachable through the API and do not fail
it: the answer counts them in `findings.unmapped`, and the command exits 0 and
prints one warning line on standard error with the ready-to-run command that
lists them, for example:

```text
{"event":"diagnostics","level":"warn","diagnostics":[{"source":"cli","key":"sync-findings","fix":"1 object in the API cannot be written to the sync directory because of its name and stays reachable only through the API; list it with gcs-mimic sync status --view findings --revision FINDINGS_REVISION --endpoint https://127.0.0.1:4443"}]}
```

It prints nothing more when the count is zero. `--require-no-findings`, a switch
that takes no value, fails such a wait as `FindingsPending` instead, with one
`error` diagnostic that says how many objects remain and names the findings view
that lists them, and no warning line. A value after it is refused with a
diagnostic that says the option takes none. A wait that fails prints an `error` diagnostic with its reason, the
recorded `paused_reason` for a pause and the remedy with the connection options
filled in, such as `gcs-mimic sync rescan --endpoint https://127.0.0.1:4443` for
a pause, `gcs-mimic sync conflicts` for conflicts and
`gcs-mimic sync status --view deletions --revision` with the revision of the
deletion set awaiting approval. The remedy of that set says how many of the
tracked files are missing, and names, in this order, the deletion view, fixing
a failed mount and restarting the server before anything else, `reject` for
files that went by mistake, restoring the files and waiting again, and
`approve` for files deleted on purpose. A remedy names the findings view and the deletion
view with the revision its answer read, and without one, such as in a log, the
view without `--revision`; every command a remedy, a log event or the
healthcheck names is rendered from one list of commands, each of which this
command's tests parse.

A wait that ends with `SourceUnrepresentable` also answers `source_entries`:
their `total`, the first five `entries` its own pass could not take in, each
with its `path` and `reason`, and how many it `omitted`. Its error names them
before the remedy, for
example `sync wait ended with SourceUnrepresentable (2 source entries:
INVALID-BUCKET [unmappable-name], INVALID-BUCKET/body [unmappable-name]): ...`.
The status reports only the scan stop and lists no entries. A wait that ends
with `DeletionApproval` answers `deletions`: the `paths` of the first ten files
of the set that waits, how many more it `omitted`, the `removed_buckets` that
went whole and `removed_buckets_omitted`, as the
`sync_deletion_approval_required` event names them; its remedy opens with why
the set waits. A path that is not
UTF-8, the kind of name a scan cannot take in, shows U+FFFD for each invalid
sequence, in the answer as in the logs and remedies that name it.

Every line the command writes from a server's answer, the entries, remedy,
findings revision and error message alike, has each control character escaped,
such as `\u{1b}` for ESC, before it reaches the terminal. A file name in the
sync directory therefore cannot move the cursor or rewrite the terminal of
whoever runs the command. The JSON documents the command prints keep the
server's strings as JSON encodes them.

Authorization and request-body preparation use the catalog-wait budget without
being mistaken for connection inactivity. After parsing authorized input, the
requested observation budget takes over. Waiting captures a fixed scan/journal fence; unrelated later
writes do not extend it indefinitely. Deadline checks run between owned
scan/export/deletion steps; settling
also observes the deadline. An interrupted scan cannot complete its baseline or
authorize deletion. The status writeback summary omits the former always-null
`journal_sequence` and `journal_epoch`; wait exposes its actual `journal_fence`.
A timeout/disconnect does not cancel an
already accepted scan or publication; inspect status before retrying.

`wait` prints its last answer as JSON whatever the outcome, and its exit status
tells the outcomes apart, so that a script or CI step need not read the answer.
The other commands exit 0 on success and 1 on any failure.

| Exit status | `sync wait` outcome | Reasons |
| --- | --- | --- |
| 0 | Every change the wait can reflect is reflected, even while objects it only reports remain | `Ready` |
| 1 | Anything else: an unreachable server, a refused request or option, a server of another release, or a server that stopped | `Stopped`, or no reason |
| 2 | Waiting does not change what holds synchronization back, so someone must act; the error names what to run | `Paused`, `SourceUnrepresentable`, `DeletionApproval`, `FindingsPending`, including a `--require-no-findings` failure |
| 3 | Work was still in progress when `--timeout` passed; the error names what remained | `Timeout`, `SourceChanging`, `JournalChanged` |

Headers are limited to 64 and 16 KiB, and both chunked and length-framed JSON
responses have a 256 KiB body cap. Malformed, truncated and oversized responses
fail. A non-JSON body reports its HTTP status without echoing the body.
Non-success HTTP statuses print their bounded JSON and fail, including
explicitly unsupported operations; the `error` diagnostic repeats the server's
`error.message`, with the connection options filled in for any command it
names. A server that refuses the connection or does not answer within the
timeout fails with the URL the command tried and what to check: that the
server is running and that `--endpoint`, or without it `GCS_MIMIC_LISTEN` and
`GCS_MIMIC_SCHEME`, give its address. Conflict resolution remains subject to the
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
cargo test --locked --offline --test it_sync sync_cli:: -- --nocapture
cargo test --locked --offline --bin gcs-mimic sync_cli
cargo test --locked --offline --lib storage::catalog::sync_state::conflicts -- --nocapture
cargo test --locked --offline --lib storage::catalog::sync_state::writeback::recovery -- --nocapture
cargo xtask lint
cargo clippy --test it_sync --bin gcs-mimic --locked --offline -- -D warnings
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

The real-server 503 timing path remains separate from the controlled
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
