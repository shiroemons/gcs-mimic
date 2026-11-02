# Differential harness verification

Verified on **2026-11-02** for DESIGN-089. This evidence covers the optional
harness workflows and the normalized fields of its 59 declarative cases;
it does not establish unrestricted Cloud Storage API parity. The committed
[reference](../../../compatibility/differential/reference/gcs.json) combines
58 cases from **2026-11-01** with `obj-resumable` re-recorded on **2026-11-02**.
The [comparison report](differential-comparison.md) retains all 59 descriptions.

## Self-check: differential_self_check

```sh
cargo xtask differential validate
cargo xtask differential run --fail-on-note
```

Validation: `59 differential cases are valid`. The final self-check returned
**59 ok, 0 notes, 0 errors, 0 skipped**. Neither command used Google credentials
or contacted the reference service. Cleanup ended with
`sweep: 0 leftover bucket(s) found`.

## Live recording: differential_live_record

After successful validation, a local `obj-resumable` self-check and an
authentication check, exactly one authorized live refresh ran with the project
and access token supplied only through environment variables. Only
`obj-resumable` was selected: **22 steps, 30 reference requests, 0 HTTP 429
retries**. No live retry was needed. The recording writer's redaction guard
accepted the partial; credentials and capability URLs were not retained.

```sh
cargo xtask differential run --filter obj-resumable \
  --record "$TMPDIR/resumable-live.json" --out "$TMPDIR/resumable-live"
```

The live run reported:

```text
reference shared bucket: emptied (0 object versions), deleted, GET confirmed 404
reference sweep: 0 leftover bucket(s) found
```

The same cleanup succeeded for the local emulator. The 2026-11-01 reference
already covered all 59 cases; the refresh changed only one case. Combined
request metadata is **811**, including the earlier superseded observations
and cleanup, rather than the number of retained request steps.

## Partial merge: differential_partial_merge

```sh
cargo xtask differential merge compatibility/differential/reference/gcs.json \
  "$TMPDIR/resumable-live.json" --out "$TMPDIR/resumable-merged-reference.json"
```

After reviewing the candidate, the same guarded merge wrote the committed
reference in place. A `jq` comparison against the pre-merge reference found
**only `obj-resumable` changed**; all other **58 cases were equal** as JSON
values, including the absence of legacy bucket-mode metadata. Both recording
dates and the pinned tool identities were retained. The merged output passed
the recording redaction/leak guard.

Regression coverage checks every committed step fingerprint against its case
under both standalone and workspace feature unification. Merge tests reject
zero-request bases even without provenance, reject self-check/failed/stale or
incompatible inputs, and preserve unselected cases and metadata. The local
Pub/Sub stub also survives an injected accept failure before serving the next
lookup.

## Offline replay: differential_offline_replay

```sh
cargo xtask differential run \
  --reference compatibility/differential/reference/gcs.json \
  --evidence docs/en/evidence/differential-comparison.md \
  --fail-on-diff --fail-on-note
```

Final result: **59 match, 0 differing fields, 0 notes, 0 errors, 0 skipped**.
No Google request was sent. Before the refresh, 58 cases matched and the
changed resumable case was the only stale-reference error. Immediately after
refresh, resumable had **16 differing fields**; the terminal-state and response
fixes reduced those to **0**. The new error state and cleanup journal survive
process-crash and power-loss snapshots; failed body writes, flushes and syncs
preserve the acknowledged prefix. Only syntax errors poison sessions;
duplicate headers and inconsistent offsets/totals stay retryable local 400.
Larger non-multiple chunks retain local error wording. XML cancellation remains
204 and gRPC retains its existing status mapping.

Focused verification passed **139 resumable library tests**, **4 signed XML
resumable integration tests** and **1 memory resumable integration test**.
The pinned Go SDK cancellation scenario also passed, including the 499 JSON
envelope on DELETE and subsequent PUT:

```sh
GCS_MIMIC_GO_TEST_RUN='^TestMigrationAppScenarioCancellation$' \
  bash scripts/sdk-go.sh
```

Unrecorded lifetime boundaries, completed-session DELETE and other resumable
error choices remain documented under SPEC-RSM-EDGES in the
[slice contract](../slices/resumable-upload.md).
