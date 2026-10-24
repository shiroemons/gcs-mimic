# Bucket initialization contract

<!-- markdownlint-configure-file {"MD013": false} -->

Contract proposed on **2026-10-24** for `bucket-initialization`, pending independent review. This slice defines a future emulator extension; it adds no runtime settings, parser or startup phase. JSON-BUCKETS-002, JSON-BUCKETS-008 and DESIGN-062 remain PARTIAL. BEHAVIOR-BUCKET-009 retains its implemented API CORS scope without new pending initializer cases. The [feature ledger](../../../compatibility/features.json) owns all status and evidence.

## Baseline, sources and ownership

This contract follows [Bucket initializer](../../../planning/onboarding-tracks-plan.ja.md#6-bucket-initializer), the shared startup/create-missing requirements and the [configuration diagnostics contract](configuration-workflows.md#shared-diagnostics-contract). Inspection of `src/config.rs`, `src/main.rs`, `src/seed.rs`, `src/json/buckets.rs`, `src/json/query.rs` and `src/bucket.rs` confirms that initialization files are not implemented. The findings' schema-13 description is historical: the current catalog is schema **14**, storing bucket/object timestamps in microseconds. No millisecond rounding is introduced.

The official [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets) and [buckets.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert) references were checked on 2026-10-24. They establish resource field names, the API's separate required project query and creation permission; they do not define an initialization file or an idempotent batch operation. The pinned JSON API v1 Discovery baseline remains revision `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`, recorded in [source-manifest.json](../../../compatibility/upstream/source-manifest.json). No protobuf or SDK contract changes.

Existing SPEC-PAP-EDGES, SPEC-CORS-EDGES, SPEC-SIGNED-BROWSER-CORS, SPEC-BUCKET-PATCH, SPEC-008, SPEC-012, SPEC-017, SPEC-FIELDS-EDGES and SPEC-JSON-ETAG-WIRE remain open in [gaps.json](../../../compatibility/gaps.json). Local administrative initialization does not resolve provider permissions, projection, timestamps/ETags, null/update/error parity or CORS/signing questions. The pinned fake-gcs-server comparison remains v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`; this file format is independent of its seed/backend formats.

`init-parser` owns bounded input and typed parsing; `init-create-missing` owns orchestration and onboarding examples after `shared-startup-phases` and `shared-admin-interface`. Shared state alone owns catalog internals and startup ordering. Configuration owns `Diagnostic` and event rendering. This slice creates only this contract, its ledger cases and generated views; it adds no dependencies or common runtime component.

## Input selection and limits

Reserve the following native server setting; it is **not accepted by the current binary**:

| Surface | Reserved input |
| --- | --- |
| CLI | `gcs-mimic serve --buckets-file PATH` |
| Environment | `GCS_MIMIC_BUCKETS_FILE=PATH` |
| Flat JSON server config | `"buckets_file":"PATH"` |
| Default | Unset; no file discovery or implicit initialization |

Use CLI > environment > config file > unset. Duplicate CLI options remain errors. An explicit empty/non-string path is invalid, not disabled. Resolve relative paths against the process working directory, matching existing file settings; do not resolve against the server config's directory. Only the selected path is opened; an overridden path is not inspected. A selected unreadable file fails startup. Open once, require a regular file (a symlink to a regular file is allowed), read at most the file limit plus one byte and close it. No stdin, URL, directory traversal, file watching or startup rewrite of input is provided. File replacement after this read takes effect on the next startup.

| Limit | Inclusive maximum | Accounting |
| --- | --- | --- |
| File | 1 MiB = 1,048,576 bytes | Original UTF-8 bytes, including whitespace; bounded read detects the next byte |
| Entries | 1000 | Elements in `buckets`, including invalid/duplicate entries |
| Entry | 64 KiB = 65,536 bytes | Original JSON object span including braces, its `project` and internal whitespace; excludes surrounding array whitespace/separators |
| Native resource | 64 KiB = 65,536 bytes | Compact JSON supplied to the native bucket parser after removing entry `project`; encoding must not bypass the API body cap |

Exactly-at-limit input is allowed if otherwise valid. A 1001st entry, oversized file or entry is an error, never truncation. The entry cap does not replace the smaller shared label/CORS/name/project limits. Empty `buckets:[]` is a valid applied no-op. Memory and diagnostic retention are bounded by the file bytes and entry count; no unbounded directory/catalog scan is allowed. These are fixed v1 limits, not new tuning options or measured performance claims.

## Version 1 schema and project resolution

The file is one UTF-8 JSON object with no BOM, comments, trailing JSON values or duplicate object member names at any depth. Required `version` is the integer token `1` (not a string, boolean, null, fraction or exponent form); unknown versions fail. Required `buckets` is an array of objects. Optional `project` values are strings; explicit null/empty never falls back. No other top-level keys are allowed.

```json
{
  "version": 1,
  "project": "local-project",
  "buckets": [
    {
      "name": "public-assets",
      "cors": [{"origin": ["http://localhost:3000"], "method": ["GET"], "maxAgeSeconds": 3600}]
    },
    {
      "name": "private-uploads",
      "project": "uploads-project",
      "versioning": {"enabled": true},
      "labels": {"env": "dev"},
      "iamConfiguration": {"publicAccessPrevention": "enforced"}
    }
  ]
}
```

For each entry use **entry `project` > file `project` > effective `seed_project`**. `seed_project` itself retains CLI > environment > server config > `local-project`. Validate every supplied project, including a file project overridden by all entries, with the shared nonempty/no-whitespace-or-control/256-byte rule. An invalid inherited project suppresses only checks that require that project; independently detectable entry problems are still reported. There is no cloud project lookup or inference from identity bindings, bucket names or projectNumber. Projects are compared exactly as validated strings.

`name` is required. Names are unique across the entire file, **including across different projects**; the second and subsequent occurrences each report the first index. Escaped equivalent names compare after JSON decoding. Invalid names receive validation diagnostics; do not silently normalize, case-fold or rename them.

The entry allowlist is `name`, `location`, `storageClass`, `labels`, `cors`, `versioning`, `defaultEventBasedHold`, `iamConfiguration`, `project`. Nested object allowlists are also closed:

| Field | Allowed shape / shared native behavior |
| --- | --- |
| `name` | Native bucket naming rules; no legacy-only names |
| `location` | `US`, `EU`, `ASIA`, case-sensitive; omitted defaults to native `US` |
| `storageClass` | Shared native supported class set; omitted defaults to `STANDARD`; no placement simulation |
| `labels` | String map, at most 64 labels, shared key/value rules; omitted empty; arbitrary valid label keys are not schema fields |
| `cors` | Array of rules or native `null` empty value; rule keys only `origin`, `method`, `responseHeader`, `maxAgeSeconds`; shared CORS validation/order and omitted-list defaults |
| `versioning` | Object with optional boolean `enabled`; omitted object/member defaults false; null is invalid |
| `defaultEventBasedHold` | Boolean; omitted false; null invalid |
| `iamConfiguration` | Object with optional `publicAccessPrevention` of `inherited` or `enforced`; omitted object/member defaults inherited; null invalid |
| `project` | Initializer-only override, removed before native bucket parsing |

The future `default_location` setting reserved by Configuration, when implemented, supplies the omitted native location for the initializer too; explicit entry location wins. Until then the default is `US`. This does not enable an otherwise unsupported global location setting in `legacy-emulator` or alter Configuration's rejection of that setting there.

Reject unknown and output-only resource fields even if the API currently ignores them. Reject `iam`, `iamPolicy`, bindings, ACL/default ACL fields and all unsupported writable fields; no IAM policy can be installed by v1. Reject `lifecycle` **even when null or `{"rule":[]}`**. Reject `iamConfiguration.uniformBucketLevelAccess` and `bucketPolicyOnly`, including `enabled:false`: the API's accepted no-op is not an effective initializer setting. Allowing `publicAccessPrevention` does not allow policy grants or UBLA transitions. No setting is accepted and silently discarded.

Use serde-derived closed structs with `deny_unknown_fields` for schema authority, including nested fixed-key objects. Remove `project`, then reuse `parse_insert_with_profile(..., CompatProfile::Native)` and shared validators for values regardless of the running compatibility/authentication profile. Do not copy business validation into a second implementation. Existing first-error deserialization/native parsing alone cannot satisfy the diagnostic contract: `init-parser` needs collection of independent structural and shared semantic errors before final conversion. A collecting validation entry point must preserve existing API acceptance/errors; its shape is for that slice to determine. This is a necessary implementation handoff, not permission to weaken API tests or change Shared state internals.

## Validation order and complete diagnostics

1. Resolve native server settings and input selection, retaining Configuration's independent CLI/environment/config/TLS/identity preflight diagnostics. An invalid selector suppresses its file read only.
2. Read the selected bounded file and validate UTF-8, JSON syntax, root shape, version and entry-count envelope. Unreadable, oversized or syntactically malformed input produces a selector/`$` diagnostic; do not fabricate field errors from unparseable content. A bad root/version/count prevents mutation but does not hide independently inspectable fields in the bounded, syntactically valid document. Unsupported version skips version-dependent entry interpretation.
3. For version 1, inspect every entry, original byte limit, repeated members, allowed fields, types, supplied projects, name validity and duplicate names. Inspect other entries even when one is invalid. Validate all independent semantic fields with shared native rules, skipping only checks whose required type/project/name input is invalid. Entries over their cap receive a limit diagnostic; inspect other entries, without passing oversized resources to native parsing.
4. If any validation error exists, return nonzero with no initializer mutation, seed, bind or ready. Validate existing-bucket entries too: invalid settings cannot be hidden by the bucket already existing. The initializer's load/validation is its bounded startup phase after catalog recovery/PAP, so catalog open/recovery may already have occurred; this is not a promise that an invalid bucket file never creates a data directory.
5. Only after successful validation invoke `ensure_buckets` once with all entries. Emit project-mismatch warnings from its results; continue to seed and later startup phases. Ready is emitted only after all required phases succeed.

Use the [shared diagnostics record and renderer](configuration-workflows.md#shared-diagnostics-contract): `source:"initializer"`, original zero-based paths such as `buckets[2].location`, and `fix` containing the reason and a safe actionable example. Selector configuration errors retain `cli`/`environment`/`config` and the original selector key. File read/syntax errors use `source:"initializer",key:"buckets_file"` (or `$` for document-root shape). Emit one bounded initializer validation batch, warnings before errors; records sort by shared source order then key UTF-8 bytes, preserving encounter order for repeated keys. No duplicate human prose line accompanies JSON. Diagnostics remain visible at every log level; any error prevents ready.

Report every independently detectable occurrence, not just the first error per entry or the first unknown key. For duplicate members use the same field path and identify the repeated occurrence in the fix; never keep only the last value. Unknown keys include their entry index and nearest allowed field in the fix (minimum character edit distance, ties by UTF-8 byte order); suggestions are advice, never automatic correction. Known unsupported keys instead say unsupported and recommend removal or a delivered API alternative. Label-map keys are data, so do not suggest schema names for them. Escape original field paths/control characters with JSON; never echo raw parser/I/O text, arbitrary values, credentials or the file's contents. Use safe examples rather than an invalid supplied project or secret-shaped value. File and entry byte limits bound complete diagnostic collection; never truncate it into success.

```json
{"event":"diagnostics","level":"error","diagnostics":[{"source":"initializer","key":"buckets[0].storageClas","fix":"Unknown field; use storageClass, for example \"storageClass\":\"STANDARD\"."},{"source":"initializer","key":"buckets[1].lifecycle","fix":"Unsupported in bucket initialization version 1; remove lifecycle, including an empty rule list."}]}
```

## Create-missing and Shared state requirements

The file is trusted, operator-controlled local administrative input, with the same authority as seed. It bypasses API principal permissions in **both strict and permissive authentication modes**, through the Shared state administrative interface; no bearer token, signer key or cloud call belongs in the file. Subsequent API operations enforce their usual profile, IAM/PAP and CORS rules. Initializing a bucket never grants anonymous access, and CORS/public URL construction never grants permission.

Required observable properties of `ensure_buckets`, to be delivered by `shared-admin-interface`:

- Accept the fully validated bounded list, with no adapter SQL or direct catalog access. Use one batched existing-name lookup and one transaction for the missing set, not a loop of 1000 separately committed creates. Batch the durability work into one commit; measure actual sync behavior rather than asserting a physical fsync syscall count from the API shape.
- Classify existence and publish missing buckets atomically, including races with other administrative callers. Return one `Created` or `Existing` result per input in input order; `Existing` must identify its actual owning project so the caller can detect mismatches without a second lookup. An existing record, policy, metageneration, timestamps, objects, versions and holds remain unchanged even if requested settings differ.
- A project mismatch is still `Existing`, counts once, changes nothing and produces a warning for that input path. It never moves a bucket or fails the initializer. Its warning fix explains that existing ownership is retained and suggests aligning/removing the entry or choosing another bucket; avoid echoing project values. Seed's separate same-project rule is unchanged: an overlapping seed directory in a different project still fails the seed phase.
- Any precommit insertion, disk-full or commit failure rolls back **all newly requested missing buckets**, leaving previously existing state untouched. Kill/restart recovery observes either the complete committed missing set or none of it, never a committed prefix. Report storage failures explicitly with the shared operational diagnostics; a failed/ambiguous operation must never be reported as applied/ready.
- Once the transaction has committed, a disconnect/shutdown, later seed failure or listener failure does not undo it. Drain in-flight side effects and close/release the catalog before stopped/exit. On retry reclassify committed entries as Existing and create only still-missing ones. A crash around commit may leave the caller uncertain; retry converges without overwriting. This resolves the plan's shorthand “interruption creates nothing” at the actual precommit boundary.

This defines required results, batching, atomicity, recovery and acceptance tests, **not** SQL schema, Rust signatures, locking/admission implementation or internal record types. Their owner chooses those. Use the injected `Clock` for new bucket creation/update timestamps, metageneration 1 and shared default policy; preserve microseconds through catalog v14 without millisecond normalization. No caller-specified timestamps, generations or policies are accepted.

The agreed startup sequence is config validation → TLS/identity → catalog recovery → import marker check → PAP → bucket initializer → seed → sync preflight → bind → ready → sync reconciliation. `shared-startup-phases` owns that sequence and cancellation/cleanup. Initializer adds its phase only through the agreed hook. No background reconciliation, polling or settings reset occurs. Removing an entry does not delete a bucket; re-adding it does not reset settings. A bucket explicitly deleted through the API can be recreated from the file on a later startup.

## Ready event

`init-create-missing` contributes only the `buckets_file` member to the existing stdout ready event; retain all other owners' keys. Unset input yields exactly:

```json
{"buckets_file":{"outcome":"disabled"}}
```

Successful application, including an empty file, yields:

```json
{"buckets_file":{"outcome":"applied","created":2,"existing":1,"project_mismatches":1}}
```

Counts are nonnegative integers; created + existing equals the validated input count, with project_mismatches ≤ existing. All three counts are present, including zeros. Mismatches additionally emit one shared stderr `warn` diagnostic per mismatching entry before ready; the count in ready makes the warning machine-visible without exposing file contents. Existing with different settings but the same project is ordinary Existing, with no warning. Never emit a failed/applied partial ready event or ready before commit, seed and listener success. No file path, raw entry, policy, token or key is included in ready. Restart counts describe that run only.

## API differences and profile interactions

This is an emulator-local startup operation outside Google API namespaces. Google `buckets.insert` has one resource and a project query, requires API creation permission and rejects an already-used live name. This contract instead has a versioned wrapper, per-entry project selection, local administrative authority, strict allowlists and an atomic idempotent create-missing batch. Its errors are startup diagnostics, not Google HTTP error envelopes. API unknown-field behavior, empty lifecycle SDK accommodation and disabled-UBLA no-op remain unchanged.

All file values follow native rules even under `compat_profile=legacy-emulator`; legacy-only names fail and labels/location/class are not discarded on creation. Active legacy **response rendering is unchanged** and can display fixed defaults for metageneration-1 resources; verify preserved native values by reopening in native and by effective CORS/versioning/hold/PAP tests rather than claiming all legacy GET fields already expose them. Metadata-only location/class do not imply placement, billing or lifecycle execution. Strict mode still requires its valid identity configuration; local administrative initialization grants no new API authority.

## Acceptance and delivery

These cases are recorded before implementation. All initializer cases remain PENDING until their actual verification passes; documentation/check success alone does not pass runtime cases or independent review. The existing API/seed cases and fidelity stay unchanged. The contract case requires independent review with no major findings plus markdownlint, ledger validate/render/check and `just check`; the integration owner runs `just check` after merging this documentation slice, without duplicating it in each worktree. The coordinator owns recording that review result.

| Ledger row / case | Required verification | Owner |
| --- | --- | --- |
| DESIGN-062 / `BUCKET-INIT-CONTRACT` | Review schema/version, selectors, limits, project precedence, full diagnostics, ready, atomicity and API/profile differences; document checks succeed without runtime completion claims | `init-contract` |
| DESIGN-062 / `BUCKET-INIT-INPUT` | CLI/env/config precedence, unset/empty/invalid path, working-directory resolution, regular/symlink/nonregular/unreadable input; UTF-8/BOM/JSON/trailing data; missing/null/wrong/unknown version/root/array; empty list | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-BOUNDS` | Exactly/one-over 1 MiB, 1000 entries and raw/encoded 64 KiB entry/resource; whitespace/escapes/project accounting; bounded read/retention and no partial success | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-VALIDATION` | All supplied and inherited projects, entry override/default, global decoded duplicate names, repeated JSON members at every depth, nested unknown/typo/output fields, iam/policy/ACL and all lifecycle shapes, disabled UBLA rejection; native semantics in both compatibility profiles | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-DIAGNOSTICS` | Multiple independent structural/semantic errors within and across entries, invalid existing entries, suggestions/ties, escaped paths, no secret/value/parser leaks; deterministic complete JSON batches, warn/error levels, no mutation/seed/bind/ready; invalid prerequisites suppress only dependent checks | `init-parser`; shared renderer by Configuration |
| DESIGN-062 / `BUCKET-INIT-ATOMICITY` | One batched lookup/transaction for 1000 records; competing callers; all-or-none failure/kill around commit, disk-full, ambiguous result and restart convergence; injected microsecond Clock; drain/shutdown removes temporary work and releases locks | Shared state; `init-create-missing` integration |
| DESIGN-062 / `BUCKET-INIT-STARTUP` | Initializer before seed, fresh/existing catalog, seed same-project behavior and separate mismatch failure, disabled/empty/applied counts, all-existing restart, project mismatch warnings, later seed/bind failure no ready without rollback of committed initialization | `init-create-missing`; Shared startup hook |
| JSON-BUCKETS-002 / `BUCKET-INIT-INDEPENDENT-SETTINGS` | Two initialized buckets independently enforce XML CORS allow/deny, version history, default holds and PAP; native metadata after legacy startup/reopen; strict/permissive subsequent API permission checks with no implicit grants | `init-create-missing` |
| JSON-BUCKETS-008 / `BUCKET-INIT-PRESERVE-API-CHANGES` | API PATCH changes, policies, object bytes/versions, conditions, timestamps and revisions survive restart and edited/removed entries; different-project recreate warns and preserves; explicit delete can be recreated on next startup | `init-create-missing` |
| DESIGN-062 / `BUCKET-INIT-ONBOARDING` | Read-only Compose file mount, native invocation, CORS/versioning journey, restart and project warning; update runnable English/Japanese guides, migration and README only when implemented | `init-create-missing` |
| DESIGN-062 / `BUCKET-INIT-RESOURCES` | Measure empty/1000-entry parsing and apply duration, CPU-seconds, peak RSS, cgroup memory and disk/sync work separately under the same backend/auth/durability profile; apply Shared state's measured regression gates when available | `init-parser`, `init-create-missing`; shared benchmark owner |

Parser/apply duration, CPU, RSS, cgroup memory, disk and sync counts are **not measured**: the parser and apply path do not exist. No container/SDK/guide execution or performance improvement is claimed by this documentation slice. Later runtime checks must use hermetic tests and the already pinned relevant SDKs; no real Google project, cloud permissions, retention locking or publication is authorized.

Documentation verification on 2026-10-24:

| Command / check | Result |
| --- | --- |
| `npx --yes markdownlint-cli2 docs/en/slices/bucket-initialization.md` | PASS, zero issues; only long-line rule MD013 disabled for prose/tables, consistent with neighboring contracts |
| `cargo xtask compatibility validate` | PASS, 591 feature rows; parameter/schema inventories remain separate |
| `cargo xtask compatibility render` | PASS, English/Japanese views regenerated with no content differences |
| `cargo xtask compatibility check` | PASS, ledger and generated views agree |
| `git diff --check` and scoped ledger comparison | PASS; only initializer cases and its delivery conditions changed; existing feature statuses/evidence and other tracks preserved |

Independent review remains pending. Full Rust tests/fmt/clippy through `just check` are delegated to the integration owner after merge; they were not run in this documentation-only worktree. SDK/container/guide and dependency freshness checks were not run here; runtime/config/dependency inputs are unchanged. The existing planning document contains the absolute-path scan command's own search patterns; this slice adds no local paths.

Next is `init-parser`, which can proceed without Shared state. `init-create-missing` waits for Shared state startup and administrative interfaces. Continuous reconciliation, IAM policy bootstrap, lifecycle, directory synchronization, source import, new settings/dependencies and other tracks' internals are outside this contract.
