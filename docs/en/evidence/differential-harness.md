# Differential harness verification

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

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

## Final workspace verification

Source: `7da2936a`, verified on **2026-11-02**. Only documentation changes
followed this verification.

- `cargo check --workspace --all-targets`: PASS.
- `cargo fmt --all -- --check`: PASS.
- `cargo xtask lint`: PASS, with its exact summary below.
- `cargo test --workspace --no-fail-fast`: PASS, exit 0.

```text
lint: 0 production violations; 1 reasoned suppressions (text::stringified_error: 1)
Workspace totals: 2122 passed; 0 failed; 1 ignored; 74 target summaries
```

The workspace total is the sum of Cargo's per-target result lines, copied
verbatim below. The one existing ignored test,
`opted_in_pubsub_emulator_receives_the_notification`, requires a separately
started local Pub/Sub emulator; no new test was ignored.

```text
test result: ok. 1076 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 141.90s
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.67s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.13s
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.61s
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.71s
test result: ok. 129 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.62s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.15s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.47s
test result: ok. 74 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.18s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.16s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.84s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.47s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.54s
test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 31.80s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.61s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.34s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.62s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.49s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.46s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.40s
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
test result: ok. 8 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.25s
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.53s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.25s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.68s
test result: ok. 39 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.52s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 14.68s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.73s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.44s
test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.71s
test result: ok. 16 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.74s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.13s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.24s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.27s
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.29s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.66s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.37s
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.05s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.70s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.74s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.03s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 27.26s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.33s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.70s
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.46s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.54s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.69s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.93s
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.52s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 176 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.86s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Markdown lint passed for the edited prose and report files. The generated
matrices were checked with MD013 disabled for their unwrapped generated text
and tables, and MD024 restricted to sibling headings: repeated category names
under different feature kinds are intentional. Their existing long-line
count did not increase. The complete guide and container/SDK suites were not
rerun; the affected Go cancellation scenario and HTTP regressions above were.
