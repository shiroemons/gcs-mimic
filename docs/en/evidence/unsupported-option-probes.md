# Provider probes for rejected options

<!-- markdownlint-configure-file {"MD013": false} -->

Recorded on **2026-11-13** (UTC). gcs-mimic answers many inputs it does not
implement with JSON 400 `badRequest` or gRPC `UNIMPLEMENTED`
(SPEC-SDK-UNSUPPORTED). These probes ask what Cloud Storage does with the same
requests, so each rejection can be judged as a provider match, a recorded local
choice or a difference to fix. They cover only the request shapes listed below.

## Live recording

One opted-in differential run recorded six new cases against real Cloud Storage,
with the project and access token supplied only through environment variables:

```sh
cargo xtask differential run --filter unsupported --record <partial-reference>
```

- Requests sent to Cloud Storage: **112**, with **0** HTTP 429 responses.
- Region: disposable us-central1 buckets, STANDARD class, soft delete off.
- Cleanup: every object and bucket the run created was deleted, and the run's
  cleanup check confirmed that nothing was left.
- Cost: the buckets held only objects of a few bytes in a free-tier US region.
  The run locked no retention policy and created no KMS key, Pub/Sub topic,
  logging sink or other billable resource; the KMS step names a key that does
  not exist.

The partial recording was merged with `--add-new` into the committed
[reference](../../../compatibility/differential/reference/gcs.json), which now
holds **108 cases** and **1546 requests**. The case definitions are in
[compatibility/differential/cases](../../../compatibility/differential/cases).

## Outcomes

"Changed" means gcs-mimic now gives the recorded answer. "Local choice" means
gcs-mimic keeps its own answer, marked per step with `local_choice` and a reason
in the case file, so the replay checks that the choice still holds.

| Case | Cloud Storage | gcs-mimic |
| --- | --- | --- |
| `bkt-unsupported-parameters` | `quotaUser` and `userIp` are ignored; `callback` returns a JSONP body; `alt=media` on a bucket or listing is a text 400 `Unsupported Output Format` | Changed: `quotaUser` and `userIp` are ignored. Local choice: `callback` stays 400 (no JSONP); `alt=media` keeps a JSON 400 |
| `bkt-unsupported-settings` | `website`, `ipFilter` and the `REGIONAL` class on the bucket, in a lifecycle action and on an object are stored; `hierarchicalNamespace` and `customPlacementConfig` on an existing bucket are ignored; `rpo` DEFAULT and ASYNC_TURBO on a regional bucket are 400 `invalid` naming the location type; a missing KMS key is 403 `forbidden` | Local choice: every one stays 400; `rpo` DEFAULT is still accepted with 200, a known difference |
| `obj-unsupported-inputs` | An object `kind` naming another resource is ignored on insert and PATCH; an empty custom metadata key is stored; an unknown `X-Goog-Hash` algorithm is ignored; `predefinedAcl` on objects.patch is applied | Changed: `kind` is ignored. Matched: the hash algorithm. Local choice: the empty key and `predefinedAcl` stay 400 |
| `obj-unsupported-derived` | `temporaryHold`, `customTime` and `destinationPredefinedAcl` succeed on copy, rewrite and compose; move succeeds in a flat-namespace bucket, also with `projection=full`; a retention on copy into a bucket without object retention is 400 `invalid` `Bucket does not have object retention enabled.` | Matched: flat-namespace move (SPEC-001). Local choice: the derived-write options, `projection=full` on move and the retention message stay as before |
| `batch-unsupported-envelopes` | A GET and a JSON body on the batch endpoint are 502 HTML pages; a body without parts is a text 400 `Batch request did not contain any valid child requests.` | Local choice: each stays a JSON 400 `badRequest` |
| `grpc-unsupported-options` | ListBuckets with `return_partial_success`, GetIamPolicy with policy version 2, nested bucket read masks, predefined ACLs on UpdateBucket, UpdateObject, ComposeObject and RewriteObject, a wildcard UpdateObject mask, `custom_time` on derived writes, compose source deletion, flat-namespace MoveObject and TestIamPermissions on an object all succeed. A mask mixing `*` with names and a wrong request checksum on ComposeObject or RewriteObject are INVALID_ARGUMENT; managed folder IAM without uniform access is FAILED_PRECONDITION. ComposeObject and MoveObject answer without `acl` and `owner`; RewriteObject includes them | Changed: `return_partial_success`, policy version 2, the mixed-mask status and message, and the ComposeObject and MoveObject owner view. Matched: source deletion and MoveObject. Local choice: the other rejections stay UNIMPLEMENTED |

On Cloud Storage the predefined bucket ACL and the wildcard object update in
`grpc-unsupported-options` succeed and change state that gcs-mimic, which
rejects them, leaves alone. The later rewrite, move and policy read in that case
therefore ignore the content type, content language and bindings in their
comparison; the case description states this.

## Not recorded

The harness refuses every retention lock request, so the answers to locking a
bucket without a policy and locking twice remain local choices under
SPEC-RETENTION-POLICY. HMAC key state errors need a key, whose creation response
carries a secret, and remain under SPEC-HMAC-WIRE. RestoreObject and Rapid
finalization owner views remain unobserved (SPEC-GRPC-DERIVED-OWNER-VIEW).

## Verification

After the changes, on the final source:

- `cargo xtask differential run --reference compatibility/differential/reference/gcs.json --fail-on-diff --fail-on-note`:
  all 108 cases match; the only listed note is the `rpo` DEFAULT local choice.
- `cargo test --locked --offline --workspace --no-fail-fast`: PASS, 3467 tests
  with no failure.
- `cargo xtask lint`, `cargo xtask compatibility check` and
  `scripts/check-markdown.sh`: PASS.
