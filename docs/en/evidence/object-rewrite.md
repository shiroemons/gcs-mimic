# Object rewrite verification

<!-- markdownlint-configure-file {"MD013": false} -->

Checked on **2026-10-23**. The [contract](../slices/object-rewrite.md) extends the
frozen basic JSON profile for `JSON-OBJECTS-012` and `BEHAVIOR-OBJECT-024`.
Implementation and state-boundary fixes are commit
`851aca26677a24582e83360f42f53de72360f0a2`.
The [ledger](../../../compatibility/features.json) remains authoritative.

## Executed checks

| Command / workflow | Result | Evidence |
| --- | --- | --- |
| `cargo fmt --all --check` | PASS | [Rust checks](object-rewrite-checks.txt) |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS | [Rust checks](object-rewrite-checks.txt) |
| `cargo test --workspace --locked --offline` | PASS, 532 tests, including 24 rewrite wire cases | [Full Rust output](object-rewrite-checks.txt) |
| `GCS_MIMIC_GO_TEST_RUN='^TestObjectRewriteCopier$' bash scripts/sdk-go.sh` | PASS, all four Copier subcases | [Pinned Go output](object-rewrite-sdk-go.txt) |

`cargo xtask compatibility render` and `cargo xtask compatibility check` pass
for all 591 rows. Standard markdownlint passes the six edited prose documents.
The two generated matrices retain their existing long table rows and repeated
category headings: default lint reports 786 MD013/MD024 issues there; the
generated-file check excludes only those two rules and passes the others.

The Go graph is Go 1.27.3, Storage v1.70.0 and API v0.308.0. The test uses
the official Copier loop and ProgressFunc. Its HTTP recorder adds the documented
one-MiB test maximum because the high-level SDK maximum field is private; it
does not replace the operation or fabricate responses. Cases cover same-location
single-call, cross-location continuation, exact boundary and empty-object
completion, source selection, metadata, hashes, destination conditions and bytes.
No Google credentials, cloud buckets or metadata-server access are used.

## Verified local behavior

- Encoded names and generation-selected sources, one-call/empty/same-name rewrite,
  inherited/replacement metadata, JSON null inheritance, supplied checksum
  rejection and path identity overriding metadata identity.
- Progress in one-MiB steps across class/location, string sizes, conditional
  resource/token presence and nested response fields. Same-location/class calls
  finish immediately regardless of a smaller test maximum.
- Random persisted request-bound tokens, omitted optional continuation fields,
  rejected changed parameters/body/path and altered keys, immediate-previous-token
  replay and concurrent retry without double advancement.
- Exactly one destination publication; completion resource and destination commit
  together. Replaying the terminal token after a later overwrite preserves the
  original response and does not create another generation. Recreated buckets
  cannot reuse an old replay record; a failed progress replay releases its active
  session, transfer, input pins and staged bytes before returning 412, then 410.
- All eight conditions, create-if-absent races, source overwrite/deletion/metadata
  changes and explicit retained source generation. Strict source read, destination
  create/delete, cross-bucket grant revocation/regrant and destination/source hold
  behavior are tested. Composite fixtures preserve component count in the same
  class/location or reset it to one when changed, with CRC32C and no MD5 resource field.
  A deterministic between-call bucket default change proves component count uses
  the class selected by the publication transaction, for both preserve/reset outcomes.
- Mid-rewrite graceful restart and process kill retain acknowledged bytes.
  Recovery removes unacknowledged tails. Missing, short and same-length corrupted
  acknowledged output is quarantined; the token fails 410 and the old destination
  remains unchanged. Foreign-key cascade removes invalid active protocol records.
- The injected clock expires sessions at the original seven-day deadline,
  including equality; active state and completed replay each have a separate
  128-record limit. The completed cache evicts old responses without rejecting
  further rewrites. Expiry removes session/transfer/input state while preserving
  live objects; startup never journals a completed destination for deletion.
  Busy expiry skips the writer and cleans other due sessions. It schedules no
  polling deadline; writer release notifies maintenance, as pinned by the test.
  The existing idle
  scheduler and blocked-seed readiness regressions pass unchanged.
- A 16 MiB+17 rewrite completes with a 1 ms catalog wait setting, even though
  streaming takes longer than that setting. A blocked initial transaction and
  streaming also finish successfully with a 1 ms request deadline: an owned
  mutation awaits its durable outcome instead of reporting a timeout while
  publication continues. Transfers use 64 KiB buffers and incremental hash state.
- Source identity/precondition failure releases active state and subsequent
  retry returns 410. Revoking overwrite delete permission leaves the destination
  unchanged and releases the failed final transfer. Every token character is
  altered in the wire test. An undelivered initial request is reclaimed, and
  deadline crossing after publication cannot change the committed success to 410.

## Final integration gate and limitations

**Pending:** the coordinator's single integrated gate after copy/rewrite/compose
merge: full SDK suite, both container architectures and native/Docker guide
execution. Those checks have not been run by this worker before source finalization.
`FINAL-INTEGRATION` remains PENDING until its shared evidence is recorded.

The frozen local session subset can then reach EXACT/IMPLEMENTED. The method row
remains PARTIAL because accepted full projection does not model ACL/owner fields
or their extra permission semantics. KMS/CSEK, requester billing, contexts,
retention and advanced metadata are explicitly rejected with documented errors.
Provider questions are outside the frozen acceptance scope and remain in
SPEC-REWRITE-EDGES and the existing basic-profile gaps: null/omission/default
details, precise token/error/header and mixed-error selection, older token replay
and exact source/precondition/permission timing.

No RSS/cgroup, CPU/throughput/latency comparison, power-loss guarantee or
all-platform filesystem guarantee is claimed. Native tests establish the named
process and state boundaries only; hard-link support on the data filesystem is
required by the shared derivation foundation. RPC, batch and enrichment rewrite
feature rows are unchanged. The next independent delivery slice is object compose;
the integrated gate remains owned by the coordinator.

Known local limitations retained after review:

- An initial session whose response is not delivered before a crash/shutdown can
  occupy a slot until expiry; the live cancellation owner has no durable delivered marker.
- Completed-cache eviction follows session creation deadline/ID rather than finish
  time. A recently finished old session can therefore lose replay on the next completion.
- The active rewrite bound shares the 128-transfer pool with copy/compose, with no
  reserved capacity; abandoned rewrites can prevent their admission until cleanup.
- A publish-time 403 discards progress under shared terminal cleanup. Grants revoked
  before adapter/source checks remain retryable, but revocation in the narrow
  check-to-publication window can require starting a new rewrite after restoration.
