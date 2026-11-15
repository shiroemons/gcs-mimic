# Provider probes for custom metadata and paired conditions

<!-- markdownlint-configure-file {"MD013": false} -->

Recorded on **2026-11-15** (UTC). These probes ask how Cloud Storage answers
inputs where gcs-mimic followed documentation or its own reading instead of a
recording: custom metadata past the documented 8 KiB, a null custom metadata
value on writes other than PATCH, a Match and NotMatch condition sent together,
and `Content-Length: 00` on a JSON method without a body. They cover only the
request shapes listed below.

## Live recordings

Two opted-in differential runs recorded the cases against real Cloud Storage,
with the project and access token supplied only through environment variables
and the case files in a directory of their own:

```sh
cargo xtask differential run --cases <case-directory> --record <partial-reference>
```

- Requests sent to Cloud Storage: **47** in the first run and **49** in the
  second, with **0** HTTP 429 responses.
- Region: disposable us-central1 buckets, STANDARD class. The shared buckets had
  soft delete off; the one bucket the restore case creates keeps soft-deleted
  objects for the 7-day minimum.
- Cleanup: every object and bucket the runs created was deleted, and each run's
  cleanup check confirmed that nothing was left.
- Cost: the buckets held only objects of a few bytes in a free-tier US region.
  The runs locked no retention policy and created no other billable resource.

The second run's answers led to the second set of cases: every write of the
first run that passed held its values at or under 8192 bytes, so the second run
tested values, keys and their totals separately. It also ran a one-off ladder of
multipart inserts with a one-byte key and values of 8,200, 9,216, 12,288,
16,384, 16,385, 32,768 and 65,536 bytes. Cloud Storage stored every one. That
case is not committed: it only bounds the observation, and its last step
deliberately differs from gcs-mimic's local bound.

Both partial recordings, without the ladder, were merged with `--add-new` into
the committed [reference](../../../compatibility/differential/reference/gcs.json),
which now holds **118 cases** and **1642 requests** (the total counts the ladder's
requests too). The case definitions are in
[compatibility/differential/cases](../../../compatibility/differential/cases).

## Outcomes

"Changed" means gcs-mimic now gives the recorded answer. "Matched" means it
already did.

| Case | Cloud Storage | gcs-mimic |
| --- | --- | --- |
| `obj-custom-metadata-limit`, `obj-custom-metadata-value-limit`, `obj-custom-metadata-resumable-past-limit`, `obj-custom-metadata-resumable-values-past-limit` | Custom metadata past 8 KiB is stored on PATCH, multipart insert, resumable start and completion and XML PUT: values of 8193 bytes under a one-byte key, an 8193-byte key, a 1024-byte key with a 7169-byte value, values that sum to 8193 across keys, and a PATCH merge past 8192 | Changed: no 8 KiB limit. Fixed and custom metadata past 40 KiB once encoded as JSON stay a local 400 `invalid`, because stored session, completion and rewrite rows keep a copy of them |
| `obj-custom-metadata-resumable-removal`, `obj-custom-metadata-null-on-write` | A null custom metadata value is stored as the empty string by a resumable start, a multipart insert, a copy, a rewrite, a compose destination and a PUT, even for a key the copy source has | Changed: only a PATCH removes a key named with null |
| `obj-generation-match-and-not-match` | Copy and rewrite evaluate `ifGenerationMatch=0` with `ifGenerationNotMatch=1`: the not-match condition fails on a destination that does not exist yet, so both answer 412 `conditionNotMet`. The source pair on a copy passes. Move refuses the pair with 409 `conflict`, `Found conflicting object preconditions. Mutually exclusive fields cannot be set simultaneously.` | Matched: copy and rewrite. Changed: move answers 409 `conflict` with that message instead of 400 `invalidArgument` |
| `obj-restore-match-and-not-match` | Restore evaluates the same pair instead of refusing it; without a live object the not-match condition fails, so it answers 412 `conditionNotMet` | Changed: the pair is evaluated instead of refused with 400 |
| `bkt-metageneration-match-and-not-match` | A bucket read evaluates `ifMetagenerationMatch` with `ifMetagenerationNotMatch` through JSON and gRPC GetBucket and answers the bucket | Matched |
| `json-content-length-zero-digits` | A bucket read, an object read and an object delete accept `Content-Length: 00` | Changed: a JSON method without a body reads the length as a number |

## Not recorded

The harness refuses every retention lock request, so whether `lockRetentionPolicy`
accepts `ifMetagenerationMatch=%2B1`, and what a native PUT with
`"retentionPolicy": {}` answers on a locked bucket, remain unobserved. A POST
policy form needs a signed policy, which the harness does not produce, so the
answer to a `Content-Language` form field remains unobserved as well.

## Verification

After the changes, on the final source:

- `cargo xtask differential run --reference compatibility/differential/reference/gcs.json --fail-on-diff --fail-on-note`:
  all 118 cases match.
- `cargo test --workspace --no-fail-fast`: PASS, 3698 tests with no failure
  and one ignored test that needs a Pub/Sub emulator.
- `cargo xtask lint`, `cargo xtask compatibility check` and markdownlint on
  the changed documents: PASS.
