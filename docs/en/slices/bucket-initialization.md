# Bucket initialization contract

<!-- markdownlint-configure-file {"MD013": false} -->

Startup application updated on **2026-10-25** for `bucket-initialization`. CLI/environment/config selectors load the bounded parser and invoke one administrative initialization transaction before seed. Updated on **2026-11-08**: an entry may write `iamPolicy`, catalog schema 52 records the entry each bucket came from, a start warns about an edited entry it did not apply, and the `buckets_file_mode=reconcile`, opt-in for `buckets_file` and the default for the inline `buckets` setting, applies written fields to existing buckets ([entry records and reconcile mode](#entry-records-and-reconcile-mode)). JSON-BUCKETS-002, JSON-BUCKETS-008 and DESIGN-062 remain PARTIAL for their broader scope. BEHAVIOR-BUCKET-009 remains IMPLEMENTED. The [feature ledger](../../../compatibility/features.json) owns all status and evidence.

## Baseline, sources and ownership

This contract follows [Bucket initializer](../../../planning/onboarding-tracks-plan.ja.md#6-bucket-initializer), the shared startup/create-missing requirements and the [configuration diagnostics contract](configuration-workflows.md#shared-diagnostics-contract). `src/bucket_init.rs` now validates initialization files independently of startup/storage. The findings' bucket validation observations remain current; its schema-13 description is historical: the current catalog is schema **14**, storing bucket/object timestamps in microseconds. No millisecond rounding is introduced.

The official [bucket resource](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets) and [buckets.insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert) references were checked on 2026-10-24. They establish resource field names, the API's separate required project query and creation permission; they do not define an initialization file or an idempotent batch operation. The pinned JSON API v1 Discovery baseline remains revision `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`, recorded in [source-manifest.json](../../../compatibility/upstream/source-manifest.json). No protobuf or SDK contract changes.

Existing SPEC-PAP-EDGES, SPEC-CORS-EDGES, SPEC-SIGNED-BROWSER-CORS, SPEC-BUCKET-PATCH, SPEC-008, SPEC-012, SPEC-017, SPEC-FIELDS-EDGES and SPEC-JSON-ETAG-WIRE remain open in [gaps.json](../../../compatibility/gaps.json). Local administrative initialization does not resolve provider permissions, projection, timestamps/ETags, null/update/error parity or CORS/signing questions. The pinned fake-gcs-server comparison remains v1.56.1, commit `896ece51e49eed0946089f3089f6343855c0d106`; this file format is independent of its seed/backend formats.

`init-parser` owns bounded input and typed parsing; `init-create-missing` owns selector configuration, orchestration and onboarding examples after `shared-startup-phases` and `shared-admin-interface`. Shared state alone owns catalog internals and startup ordering. Configuration owns `Diagnostic` and event rendering. `src/bounded_file.rs` supplies the common `take(limit + 1)` reader used by configuration, identity, TLS, signing keys and initialization. Existing stream/FIFO inputs remain accepted by the original callers. Only the initializer requires a regular file, checked on its opened descriptor; Unix uses `O_NONBLOCK` to reject a FIFO without waiting or reopening the path. Existing `serde_json` enables `raw_value` for original spans/repeated members. Approved `libc` 0.2.191 is reused for that Unix open flag; no new package or package version is required.

## Input selection and limits

The following selectors are implemented together with startup application:

| Surface | Input |
| --- | --- |
| CLI | `gcs-mimic serve --buckets-file PATH`, or the document itself with `--buckets JSON` |
| Environment | `GCS_MIMIC_BUCKETS_FILE=PATH`, or `GCS_MIMIC_BUCKETS=JSON` |
| Flat JSON server config | `"buckets_file":"PATH"`, or `"buckets"` holding the document object or a string that contains it |
| Default | Unset; no file discovery or implicit initialization |
| Mode | `--buckets-file-mode`, `GCS_MIMIC_BUCKETS_FILE_MODE` or `"buckets_file_mode"`: `create` or `reconcile`, with the same precedence. The default is `create` for `buckets_file` and `reconcile` for the inline `buckets` ([why](#entry-records-and-reconcile-mode)); an explicit value wins for both. It governs both document settings, and keeps its name. Naming a mode without `buckets_file` or `buckets` fails startup with a diagnostic for the mode's key. |

Use CLI > environment > config file > unset. Duplicate CLI options remain errors. An explicit empty/non-string path is invalid, not disabled.

The inline `buckets` setting carries the same document, with the same limits and validation, so a Compose file can hold it in `environment:`. A document has one source: naming `buckets` and `buckets_file` together fails startup with one diagnostic, keyed to the `buckets` input, that names both settings, whatever their sources. A config file object member keeps its text as written, so repeated members inside it are still reported; a member that is neither an object nor a string fails with a diagnostic for its key. gcs-mimic reads the value as plain text. Compose expands `${VAR}` before the server sees it, so a literal `$` is written `$$`. An empty value is invalid. Resolve relative paths against the process working directory, matching existing file settings; do not resolve against the server config's directory. Only the selected path is opened; an overridden path is not inspected. A selected unreadable file fails startup. Open once, require a regular file (a symlink to a regular file is allowed), read at most the file limit plus one byte and close it. No stdin, URL, directory traversal, file watching or startup rewrite of input is provided. File replacement after this read takes effect on the next startup.

| Limit | Inclusive maximum | Accounting |
| --- | --- | --- |
| File | 1 GiB = 1,073,741,824 bytes; with a named `max_buckets_per_batch`, 1 KiB per allowed bucket and at least 32 MiB = 33,554,432 bytes | Original UTF-8 bytes, including whitespace; bounded read detects the next byte |
| Entries | Unlimited, or a named `max_buckets_per_batch` (`0..=2^64−1`; default `0`, unlimited) | Elements in `buckets`, including invalid/duplicate entries |
| Entry | 16 MiB = 16,777,216 bytes | Original JSON object span including braces, its `project` and internal whitespace; excludes surrounding array whitespace/separators |
| Native resource | 16 MiB = 16,777,216 bytes | Compact JSON supplied to the native bucket parser after removing entry `project`; encoding must not bypass the API body cap |

Exactly-at-limit input is allowed if otherwise valid. An entry past the limit, an oversized file or an oversized entry is an error, never truncation, and each failure names `max_buckets_per_batch` as the fix where raising it would help. The limit bounds one file or one seed, not the buckets a server holds: Cloud Storage documents no limit on buckets per project, and under a named limit the file limit follows the setting so a fixed size does not become the real limit, with a floor that leaves room beside one entry of the largest size. Without one, 1 GiB still stops a mistaken path to a huge file, because the whole file is read into memory. The entry cap does not replace the smaller shared label/CORS/name/project limits. Empty `buckets:[]` is a valid applied no-op. Memory and diagnostic retention are bounded by the file bytes and entry count; no unbounded directory/catalog scan is allowed. These are not measured performance claims.

Validation inspects every entry, or at most the first named `max_buckets_per_batch` entries, including duplicate-member traversal. An over-count error states how many remaining entries were not inspected; this always fails and never returns a prefix of records. Diagnostic output is separately capped at **256 records and 64 KiB of rendered JSON**, including its summary and event envelope. File-level errors have 16 detailed slots and 8 KiB; entry details have up to 239 slots and 44 KiB. Each detail is limited to 2 KiB, and 12 KiB is reserved for one aggregate summary plus JSON envelope/separators. The retention rules are:

1. `Collected::record` applies the four-detail cap to every normalized parent-path/reason group, including all arrays and repeated members. Remove a terminal numeric array index; replace other nested numeric indices with `[*]`, preserving the bucket index. Thus scalar rules group as `buckets[0].cors` plus native-value, and unknown rule fields as `buckets[0].cors[*].x` plus unknown-field. Quoted field names remain literal. Up to 1024 digest/counter groups are retained; when that metadata budget fills, new groups retain only otherwise eligible priority details and still count every omitted error.
2. Each bucket's first retained detail, each known root key/reason's first retained detail and each unknown root name's first retained detail have priority. Priority is consumed only after successful retention. Known root structure failures outrank root-name advice; both outrank duplicate/extra noise. Details above 2 KiB shorten their key to a UTF-8-safe 256-byte prefix and use the fixed actionable shortening message. They remain explicit errors rather than silently consuming a priority slot.
3. The aggregate ranges identify only buckets with errors but **no retained detail**, using inclusive paths such as `buckets[12..980]`. It also reports exact omitted-detail counts and fixed reason-category counts; all buckets with details are excluded from these ranges.
4. Retention limits use named record-count/byte fields. Evictions are planned and applied only once the candidate is proven to fit. The summary bound is a compile-time calculation: at most 500 ranges, each allowing both three-digit endpoints, plus three sets of eight reason/count pairs, fixed record overhead and the event/separator margin. On 64-bit targets the conservative bound is 11,484 bytes, below the 12-KiB reservation. Const assertions and per-summary `debug_assert!` checks enforce it.

Every inspected bucket's error presence and retained-detail presence use bounded metadata. The shared `Diagnostic` summary uses `source:"initializer",key:"$"`; its ranges and counts never contain input values. Thus repeated rule scalars, unknown rule fields or repeated members cannot erase another field's distinct diagnostic. Shared sorting places the summary key before bucket paths. No new event schema, warning level or alternate renderer is introduced.

This explicitly refines the earlier complete-diagnostics requirement: bounded file size alone permitted excessive diagnostic amplification and did not satisfy the plan's count-bounded aggregation requirement. Complete detection within the inspected entries continues, but retained output is bounded and any omission remains a validation error. A stress fixture containing 15 near-64-KiB CORS entries asserts exact omitted counts, retained record/string allocation bounds and rendered bytes; it is not a process RSS or performance measurement.

For native-accepted schema values, compact JSON cannot be larger than its original entry span: whitespace and redundant escapes only shrink, and accepted numeric fields are integers. The encoded limit therefore also has an isolated defensive-guard test with valid resources at the limit and one byte past it; the public parser separately proves successful exact-limit input and raw-limit rejection. Floating `1e3` remains an invalid integer-field test rather than a false successful-boundary example.

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

The entry allowlist is `name`, `location`, `storageClass`, `labels`, `cors`, `versioning`, `defaultEventBasedHold`, `iamConfiguration`, `softDeletePolicy`, `iamPolicy`, `project`. Nested object allowlists are also closed:

| Field | Allowed shape / shared native behavior |
| --- | --- |
| `name` | Native bucket naming rules; no legacy-only names |
| `location` | Case-insensitive official regional/predefined dual-region names and `US`, `EU`, `ASIA`; omitted/null/empty uses `default_location` (default `US`). See [location contract](sdk-regional-locations.md). |
| `storageClass` | Shared native supported class set; omitted defaults to `STANDARD`; no placement simulation |
| `labels` | String map, at most 64 labels, shared key/value rules; omitted empty; arbitrary valid label keys are not schema fields |
| `cors` | Array of rules or native `null` empty value; rule keys only `origin`, `method`, `responseHeader`, `maxAgeSeconds`; shared CORS validation/order and omitted-list defaults |
| `versioning` | Object with optional boolean `enabled`; omitted object/member defaults false; null is invalid |
| `defaultEventBasedHold` | Boolean; omitted false; null invalid |
| `iamConfiguration` | Object with optional `publicAccessPrevention` of `inherited` or `enforced`; omitted object/member defaults inherited; null invalid |
| `softDeletePolicy` | Object with optional `retentionDurationSeconds` (0 or 1 to 3155760000, as an integer or string), validated like the JSON API; `effectiveTime` is output-only and rejected. Omitted takes `default_soft_delete_retention_seconds`, which is off unless set; an explicit 0 opts the bucket out of that default. |
| `iamPolicy` | Object with only `bindings`, an array of `{"role", "members"}` objects, validated with the same role, member and 1,500-occurrence rules as `setIamPolicy` ([bucket IAM](bucket-iam.md)); `allUsers` and `allAuthenticatedUsers` are allowed. A binding with an empty `members` array is dropped, as `setIamPolicy` drops it. `version`, `etag`, `auditConfigs` and binding `condition` are rejected: the catalog owns the version and etag. The written policy **replaces** the bucket's whole policy, as `setIamPolicy` does, so the default legacy bindings stay only when the entry writes them; the bucket ACL is derived from it as a policy write derives it. Omitted keeps the default policy of a new bucket. |
| `project` | Initializer-only override, removed before native bucket parsing |

An `iamPolicy` that grants `allUsers` or `allAuthenticatedUsers` while public access prevention is enforced, by the entry's `iamConfiguration.publicAccessPrevention`, by the stored bucket in reconcile mode or by the parent project's local policy, fails startup with a `buckets[i].iamPolicy` diagnostic before any bucket changes. The file keeps `"version": 1`: a release without `iamPolicy` rejects the member as unsupported, so a file that uses it never loses its grants silently, and a file without it behaves as before.

The `default_location` setting supplies the omitted/null/empty native location for the initializer; nonempty entry location wins. The default is `US`. This does not enable a global location setting in `legacy-emulator` or alter Configuration's rejection of that setting there.

Reject unknown and output-only resource fields even if the API currently ignores them. Reject `iam` and entry-level `bindings`, whose fix points to `iamPolicy.bindings`, ACL/default ACL fields and all unsupported writable fields; a policy is installed only through `iamPolicy`. Reject `lifecycle` **even when null or `{"rule":[]}`**. Reject `iamConfiguration.uniformBucketLevelAccess` and `bucketPolicyOnly`, including `enabled:false`: the API's accepted no-op is not an effective initializer setting. Allowing `publicAccessPrevention` does not allow UBLA transitions. No setting is accepted and silently discarded.

Serde-derived closed structs with `deny_unknown_fields` govern field membership, including nested fixed-key objects. The field list supplied by their derived deserializers is reused for membership and nearest-name suggestions; there is no separately maintained allowlist. Borrowed `RawValue` members retain every occurrence and exact source spans during collection. The collector invokes native parsing on independent fields, nested settings, labels and CORS elements with safe valid prerequisites. After validation, the full document is deserialized into typed bucket/CORS/versioning/PAP structures, `project` is removed and each typed resource goes through `parse_insert_with_profile(..., CompatProfile::Native)` for conversion. No API validator or acceptance behavior changes. This collects independent failures without copying native business rules or allowing an invalid project/name to hide unrelated errors.

The handoff is `bucket_init::load(&Path, seed_project: &str, &dyn Clock)` or `bucket_init::parse(&[u8], seed_project: &str, &dyn Clock)`, returning `Result<Vec<InitEntry>, Diagnostics>`: each entry carries the native bucket record, its optional policy and the set of members it writes. Any error returns no entries. Policy diagnostics name the narrowest key, such as `buckets[0].iamPolicy.bindings[1].members[0]`, and never repeat a role or member. The parser has no catalog, seed or listener access and cannot publish partial state. It samples the injected `Clock` once for the batch and preserves its microsecond timestamps. Its production module denies `clippy::unwrap_used` and `clippy::expect_used`; test-only exceptions remain inside the test module. The future caller must retain the returned diagnostics and stop its phase on error.

## Validation order and complete diagnostics

1. Resolve native server settings and input selection, retaining Configuration's independent CLI/environment/config/TLS/identity preflight diagnostics. An invalid selector suppresses its file read only.
2. Read the selected bounded file and validate UTF-8, JSON syntax, root shape, version and entry-count envelope. Unreadable, oversized or syntactically malformed input produces a selector/`$` diagnostic; do not fabricate field errors from unparseable content. A bad root/version/count prevents mutation but does not hide independently inspectable fields in the bounded, syntactically valid document. Unsupported version skips version-dependent entry interpretation.
3. For version 1, inspect every entry, up to a named `max_buckets_per_batch`, original byte limits, repeated members, closed fields, types, supplied projects, name validity and duplicate names. Inspect other entries even when one is invalid. Oversized entries still register decoded names and collect closed-schema/duplicate errors, including nested objects; skip only their native value parsing. Validate independent semantic fields of bounded entries with shared native rules, skipping only checks whose prerequisites are invalid. Diagnostics after the retention cap are counted and summarized.
4. If any validation error exists, return nonzero with no initializer mutation, seed, bind or ready. Validate existing-bucket entries too: invalid settings cannot be hidden by the bucket already existing. The initializer's load/validation is its bounded startup phase after catalog recovery/PAP, so catalog open/recovery may already have occurred; this is not a promise that an invalid bucket file never creates a data directory.
5. Only after successful validation run one administrative transaction with all entries. It observes every named bucket, plans each entry against that state and applies the plan. A plan that refuses an entry, such as a public `iamPolicy` under public access prevention, reports every refused entry and returns before any write. Emit project-mismatch and unapplied-entry warnings from the plan; continue to seed and later startup phases. Ready is emitted only after all required phases succeed.

Use the [shared diagnostics record and renderer](configuration-workflows.md#shared-diagnostics-contract): `source:"initializer"`, original zero-based paths such as `buckets[2].location`, and `fix` containing the reason and a safe actionable example. Selector configuration errors retain `cli`/`environment`/`config` and the original selector key. File read/syntax errors use `source:"initializer",key:"buckets_file"` (or `$` for document-root shape); the same errors for the inline document use `key:"buckets"` and call it a document rather than a file. Entry paths are the same for both, and no diagnostic repeats the document text. Emit one bounded initializer validation batch, warnings before errors; records sort by shared source order then key UTF-8 bytes, preserving encounter order for repeated keys. No duplicate human prose line accompanies JSON. Diagnostics remain visible at every log level; any error prevents ready.

Detect every independent occurrence in the inspected entries, rather than stopping at the first field error; retain or count it under the diagnostic limits above. For duplicate members use the same field path and identify the repeated occurrence in the fix; never keep only the last value. Unknown keys include their entry index and nearest allowed field in the fix (minimum character edit distance, ties by UTF-8 byte order); suggestions are advice, never automatic correction. Known unsupported keys instead say unsupported and recommend removal or a delivered API alternative. Label-map keys are data, so do not suggest schema names for them. Escape original field paths/control characters with JSON; never echo raw parser/I/O text, arbitrary values, credentials or the file's contents. Use safe examples rather than an invalid supplied project or secret-shaped value. Omission never turns validation failure into success.

```json
{"event":"diagnostics","level":"error","diagnostics":[{"source":"initializer","key":"buckets[0].storageClas","fix":"Unknown field; use storageClass, for example \"storageClass\":\"STANDARD\"."},{"source":"initializer","key":"buckets[1].lifecycle","fix":"Unsupported in bucket initialization version 1; remove lifecycle, including an empty rule list."}]}
```

## Create-missing and Shared state requirements

The file is trusted, operator-controlled local administrative input, with the same authority as seed. It bypasses API principal permissions in **both strict and permissive authentication modes**, through the Shared state administrative interface; no bearer token, signer key or cloud call belongs in the file. Subsequent API operations enforce their usual profile, IAM/PAP and CORS rules. Initializing a bucket never grants anonymous access, and CORS/public URL construction never grants permission.

Observable properties of the initializer's administrative transaction. Seed and reseed keep the separate `ensure_buckets` interface, whose meaning is unchanged:

- Accept the fully validated bounded list, with no adapter SQL or direct catalog access. Use one batched existing-name lookup and one transaction for the whole run, not a loop of separately committed writes. Batch the durability work into one commit; measure actual sync behavior rather than asserting a physical fsync syscall count from the API shape.
- Classify existence and publish missing buckets atomically, including races with other administrative callers: the plan sees the state its writes apply to. Count each input once as created or existing; an existing bucket's actual owning project is compared with the entry's. In create mode an existing record, policy, metageneration, timestamps, objects, versions and holds remain unchanged even if requested settings differ. A new bucket starts with the entry's `iamPolicy` instead of the default policy, stored in its creation transaction, and keeps metageneration 1.
- A project mismatch is still `Existing`, counts once, changes nothing and produces a warning for that input path. It never moves a bucket or fails the initializer. Its warning fix explains that existing ownership is retained and suggests aligning/removing the entry or choosing another bucket; avoid echoing project values. Seed's separate same-project rule is unchanged: an overlapping seed directory in a different project still fails the seed phase.
- Any precommit insertion, disk-full or commit failure rolls back **all newly requested missing buckets**, leaving previously existing state untouched. Kill/restart recovery observes either the complete committed missing set or none of it, never a committed prefix. Report storage failures explicitly with the shared operational diagnostics; a failed/ambiguous operation must never be reported as applied/ready.
- Once the transaction has committed, a disconnect/shutdown, later seed failure or listener failure does not undo it. Drain in-flight side effects and close/release the catalog before stopped/exit. On retry reclassify committed entries as Existing and create only still-missing ones. A crash around commit may leave the caller uncertain; retry converges without overwriting. This resolves the plan's shorthand “interruption creates nothing” at the actual precommit boundary.

This defines required results, batching, atomicity, recovery and acceptance tests, **not** SQL schema, Rust signatures, locking/admission implementation or internal record types. Their owner chooses those. Use the injected `Clock` for new bucket creation/update timestamps, metageneration 1 and shared default policy; preserve microseconds through catalog v14 without millisecond normalization. No caller-specified timestamps or generations are accepted; a policy comes only from `iamPolicy`.

The agreed startup sequence is config validation → TLS/identity → catalog recovery → import marker check → PAP → bucket initializer → seed → sync preflight → bind → ready → sync reconciliation. `shared-startup-phases` owns that sequence and cancellation/cleanup. Initializer adds its phase only through the agreed hook. No background reconciliation, polling or file watching occurs: reconcile mode acts only during that startup phase. Removing an entry does not delete a bucket; in create mode re-adding it does not reset settings. A bucket explicitly deleted through the API can be recreated from the file on a later startup.

## Entry records and reconcile mode

Catalog schema 52 adds `bucket_file_entries`, which records for each bucket the entry it was created or last reconciled from, or found to match, as a SHA-256 digest, together with the set of members the latest recorded entry wrote, one bit per member. The digest covers the members the entry writes, normalized as stored: sorted keys and label keys, the canonical location, the storage class name, the soft delete retention as an integer, and the policy's grants merged per role with members sorted and repeats removed. `"us"` and `"US"`, `"5"` and `5`, or reordered bindings therefore give one digest. It leaves out `name`, which selects the bucket, and `project`: ownership never changes, has its own mismatch warning, and moving `project` between an entry and the file root would otherwise look like an edit. A null member, an empty `location`, `versioning:{}`, `iamConfiguration:{}` and `softDeletePolicy:{}` write nothing. A row references its bucket with `ON DELETE CASCADE`, so deleting, soft deleting or resetting a bucket removes it in the same transaction, and a restored or recreated bucket starts without one. A catalog at schema 51 migrates automatically and one way when it opens; releases that support only schema 51 can no longer open the data directory.

`create` is the default for `buckets_file`. The inline `buckets` setting defaults to `reconcile` instead: its document lives in the file that starts the server, such as `compose.yaml`, which is then the source of truth, so an edit there applies at the next start. Members an entry does not write keep their API values in both modes. An explicit `buckets_file_mode` wins for either setting, and its name is kept although it governs the inline form too.

In `create` mode, an existing bucket owned by the entry's project is never changed:

- Without a recorded entry (created through the API, or by a release before schema 52 such as 0.13.0 on the first start after an upgrade) each written member is compared with the bucket. A difference emits one stderr `warn` diagnostic for `buckets[i]` naming the differing members and counts in `unapplied` on that start; either way the current entry becomes the record, so the warning is not repeated.
- With the same recorded entry nothing happens, even when the API changed the bucket.
- With a different recorded entry, each written member is compared with the bucket. If the bucket already holds every written value, for example because the API was used to match the edit, the new entry becomes the record silently. Otherwise one stderr `warn` diagnostic for `buckets[i]` lists the differing members by field name, never by value, and suggests `buckets_file_mode=reconcile`, an API change or restoring the entry; the old digest stays, so the warning repeats on every start until one of those happens. A differing `location` adds that an existing bucket keeps its location.

In both modes, a member that the recorded entry wrote and the current entry omits emits one `warn` diagnostic for `buckets[i]`, such as "iamPolicy is no longer written; the bucket keeps its current value. Write the value you want, or change it through the API.", counts in `unapplied` and is recorded, so it is reported on the start that first sees it only. Removing a member never resets the bucket: dropping an `allUsers` `iamPolicy` from an entry to test denied access leaves the bucket public until the entry writes the policy it wants, for example `"iamPolicy":{"bindings":[]}`, or the API changes it. The same holds for `cors`, `iamConfiguration.publicAccessPrevention` and every other member.

The public access prevention check judges the entry's whole written `iamPolicy`, whether or not it differs from the stored one, so one file gets one verdict on every volume. In create mode it uses the prevention the entry writes and the parent project's, as for a bucket being created; in reconcile mode it uses the prevention as it stands once the entry is applied, which is the stored bucket's when the entry does not write it.

`reconcile` mode applies, for each existing bucket owned by the entry's project, exactly the members the entry writes whose stored value differs, whether or not the entry changed: an API change to a written member is undone, and one to any other member stays.

| Member | Applied as |
| --- | --- |
| `labels` | A PATCH that replaces the whole label map |
| `cors` | A PATCH that replaces the rules |
| `versioning.enabled` | A PATCH of the value, which also makes the setting visible on a bucket without one |
| `defaultEventBasedHold`, `iamConfiguration.publicAccessPrevention`, `storageClass` | A PATCH of the value |
| `softDeletePolicy.retentionDurationSeconds` | A PATCH of the retention, with the effective time a PATCH gives it |
| `iamPolicy` | A `setIamPolicy` that replaces the whole policy and derives the bucket ACL |
| `location` | Never: a different location emits a `buckets[i].location` warning and the bucket keeps its own |

Only differing members are written, so a start with an unchanged file changes nothing: no metageneration, `updated` time, policy etag, ACL or journal entry moves. A bucket that changes advances its metageneration once, even when both its metadata and its policy change, and journals `bucket-update` and `iam` entries with origin `bucket-init`, as the API paths do; bucket metadata changes publish no Pub/Sub notification, through the API or here. The current entry becomes the bucket's record, except when its location was kept: the bucket then still differs from the entry, so its digest stays and a later create-mode start keeps reporting the location, while only the recorded members move. Every entry is planned against stored state before any write: a public policy that public access prevention refuses, as it stands once the entry is applied, fails startup naming `buckets[i].iamPolicy` and nothing changes. An entry for another project's bucket keeps its project warning and changes nothing. Strict mode uses the same local administrative authority as creation, without tokens. With `backend=memory` the catalog starts empty on every start, so the file creates every bucket again and reconcile mode has nothing existing to apply.

## Ready event

`init-create-missing` contributes only the `buckets_file` member to the existing stdout ready event; retain all other owners' keys. The member keeps that name for compatibility when the inline `buckets` setting supplied the document, and reports the same counts. Unset input yields exactly:

```json
{"buckets_file":{"outcome":"disabled"}}
```

Successful application, including an empty file, yields:

```json
{"buckets_file":{"outcome":"applied","created":2,"existing":1,"project_mismatches":1,"unapplied":0,"reconciled":0}}
```

Counts are nonnegative integers; created + existing equals the validated input count, with project_mismatches + unapplied ≤ existing and project_mismatches + reconciled ≤ existing. All five counts are present, including zeros; `unapplied` and `reconciled` were added on 2026-11-08 and are additive. `unapplied` counts existing buckets that were warned about a written member they do not hold or a member their entry stopped writing, once per bucket however many warnings it gets: in create mode an entry changed since it was recorded, or a first comparison without a record, and in both modes a removed member; in reconcile mode a kept location. `reconciled` counts existing buckets that reconcile mode changed. A partially applied bucket counts in both: reconcile mode can change its other members while keeping its location, so unapplied + reconciled can exceed existing − project_mismatches. Mismatches and unapplied entries additionally emit `warn` diagnostics on stderr before ready; the counts in ready make the warnings machine-visible without exposing file contents. Existing with different settings but the same project and an unchanged entry is ordinary Existing, with no warning. Never emit a failed/applied partial ready event or ready before commit, seed and listener success. No file path, raw entry, policy, token or key is included in ready. Restart counts describe that run only.

## API differences and profile interactions

This is an emulator-local startup operation outside Google API namespaces. Google `buckets.insert` has one resource and a project query, requires API creation permission and rejects an already-used live name. This contract instead has a versioned wrapper, per-entry project selection, local administrative authority, strict allowlists and an atomic idempotent create-missing batch. Its errors are startup diagnostics, not Google HTTP error envelopes. API unknown-field behavior, empty lifecycle SDK accommodation and disabled-UBLA no-op remain unchanged.

All file values follow native rules even under `compat_profile=legacy-emulator`; legacy-only names fail and labels/location/class are not discarded on creation. Active legacy **response rendering is unchanged** and can display fixed defaults for metageneration-1 resources; verify preserved native values by reopening in native and by effective CORS/versioning/hold/PAP tests rather than claiming all legacy GET fields already expose them. Metadata-only location/class do not imply placement, billing or lifecycle execution. Strict mode still requires its valid identity configuration; local administrative initialization grants no new API authority.

## Acceptance and delivery

Startup integration acceptance (recorded before implementation on 2026-10-25): enable all three selectors with existing configuration precedence; load and validate before one administrative batch; report disabled/applied counts and project warnings; preserve API mutations and objects across restart; fail without ready on validation, seed or bind failure; exercise independent CORS/versioning/holds/PAP, both auth modes, native preservation under legacy startup, and a read-only Compose mount. Performance measurement is excluded by the slice's delivery scope; no resource or timing claim will be made.

These cases were recorded before implementation. Parser and startup process tests now cover selection, complete diagnostics without seed/bind/ready, independent settings, API changes, projects, auth/profile interactions and later seed/bind failure with committed initialization retained. The existing API/seed cases and fidelity stay unchanged. Broader shared-state fault-injection cases stay explicitly pending where not covered by its existing tests. Matching English/Japanese onboarding describes the supported subset.

| Ledger row / case | Required verification | Owner |
| --- | --- | --- |
| DESIGN-062 / `BUCKET-INIT-CONTRACT` | Review schema/version, selectors, limits, project precedence, full diagnostics, ready, atomicity and API/profile differences; document checks succeed without runtime completion claims | `init-contract` |
| DESIGN-062 / `BUCKET-INIT-INPUT` | PASS selector parsing/precedence, unset/empty/non-string/invalid path and selected-path-only startup loading | `init-create-missing` |
| DESIGN-062 / `BUCKET-INIT-PARSER-INPUT` | Working-directory paths, regular/symlink/nonregular/unreadable input; UTF-8/BOM/JSON/trailing data; missing/null/wrong/unknown version/root/array; empty list | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-BOUNDS` | Exactly/one-over the file limit, a named `max_buckets_per_batch` and raw/encoded 16 MiB entry/resource; whitespace/escapes/project accounting; bounded read/retention and no partial success | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-VALIDATION` | All supplied and inherited projects, entry override/default, global decoded duplicate names, repeated JSON members at every depth, nested unknown/typo/output fields, iam/policy/ACL and all lifecycle shapes, disabled UBLA rejection; native semantics in both compatibility profiles | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-PARSER-LINT` | The parser module denies `clippy::unwrap_used` and `clippy::expect_used`; exceptions are confined to its test module; clippy passes without changing API validation | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-PROPERTY` | Arbitrary bytes and JSON shapes/depths do not panic; generated valid names must succeed with input names/count/order; known-schema value combinations cannot fail in typed conversion at `$` | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-SHARED-READ` | Generic bounded-reader inclusive/overflow behavior plus config/signing FIFO inputs; private key at 16,384 bytes succeeds and 16,385 fails explicitly | `init-parser` |
| DESIGN-062 / `BUCKET-INIT-DIAGNOSTICS` | Multiple independent structural/semantic errors, suggestions/ties and escaped paths without value leaks; bounded shared JSON batches and exact omitted count; over-count and oversized entries follow the bounded inspection policy; startup suppression awaits integration | `init-parser`; shared renderer by Configuration |
| DESIGN-062 / `BUCKET-INIT-ATOMICITY` | One batched lookup/transaction for a full batch of records; competing callers; all-or-none failure/kill around commit, disk-full, ambiguous result and restart convergence; injected microsecond Clock; drain/shutdown removes temporary work and releases locks | Shared state; `init-create-missing` integration |
| DESIGN-062 / `BUCKET-INIT-STARTUP` | Initializer before seed, fresh/existing catalog, seed same-project behavior and separate mismatch failure, disabled/empty/applied counts, all-existing restart, project mismatch warnings, later seed/bind failure no ready without rollback of committed initialization | `init-create-missing`; Shared startup hook |
| JSON-BUCKETS-002 / `BUCKET-INIT-INDEPENDENT-SETTINGS` | Two initialized buckets independently enforce XML CORS allow/deny, version history, default holds and PAP; native metadata after legacy startup/reopen; strict/permissive subsequent API permission checks with no implicit grants | `init-create-missing` |
| JSON-BUCKETS-008 / `BUCKET-INIT-PRESERVE-API-CHANGES` | API PATCH changes, policies, object bytes/versions, conditions, timestamps and revisions survive restart and edited/removed entries; different-project recreate warns and preserves; explicit delete can be recreated on next startup | `init-create-missing` |
| DESIGN-062 / `BUCKET-INIT-ONBOARDING` | Read-only Compose file mount, native invocation, CORS/versioning journey, restart and project warning; update runnable English/Japanese guides, migration and README only when implemented | `init-create-missing` |
| DESIGN-062 / `BUCKET-INIT-IAM-POLICY` | `iamPolicy` keeps written bindings, drops empty ones and refuses version/etag/auditConfigs/conditions, unknown members and invalid roles/members at their keys without echoing values; a new bucket's policy and ACL equal those after the same `setIamPolicy`, at metageneration 1; strict anonymous reads follow it and a bucket without one stays private; entry or parent-project public access prevention refuses a public policy before any bucket is created; memory backend | `bucket-initialization` |
| JSON-BUCKETS-008 / `BUCKET-INIT-UNAPPLIED` | An edited entry warns on every start, without values, until the bucket matches; API-only changes and unchanged entries stay silent; a bucket without a record reports its differences once and adopts the entry; a member the entry stops writing is reported once in both modes and never reset; a bucket counts once in `unapplied`; equivalent spellings share a digest; deleting, soft deleting or restoring a bucket forgets its record; schema 51 catalogs and released data directories upgrade | `bucket-initialization` |
| JSON-BUCKETS-008 / `BUCKET-INIT-RECONCILE` | Reconcile mode applies only differing written members, undoes API changes to them and keeps others, advances metageneration once per changed bucket, writes nothing on an unchanged restart, keeps location and another owner's bucket with warnings and keeps the digest of a bucket whose location it kept, so create mode keeps reporting it; a public policy is judged whole against public access prevention on every volume before any write; works in strict mode without tokens; the mode without a buckets file fails startup | `bucket-initialization` |

Parser/apply duration, CPU, RSS, cgroup memory, disk and sync counts are not measured. The current slice explicitly excludes benchmarking and resource evidence; the older measurement requirement is superseded by that delivery scope. Bounded input and one administrative transaction remain design requirements. No real Google project, cloud permissions, retention locking or publication is authorized.

Verification on 2026-10-25 against the integrated source-import, public-origin and startup-phase baseline:

| Command / check | Result |
| --- | --- |
| `just check` | PASS, exit 0; formatting, workspace clippy/tests, ledger/generated views and hermetic shell/publication checks; zero test failures |
| `cargo test --offline --test bucket_initialization` | PASS, 7 process tests: settings/CORS/versioning/holds/PAP, restart metadata/policy/bytes/versions/conditions, project ownership/recreation/warnings, complete diagnostics, later seed/bind failures, strict/legacy and selectors |
| `cargo test --offline --bin gcs-mimic startup::tests` | PASS, 8 tests, including interruption during initializer validation without publication |
| `cargo test --offline --test foundation` | PASS, 19 tests, including the additive ready key and existing startup/shutdown behavior |
| `cargo test --offline --lib bucket_initializer_selectors` | PASS, configuration selection and empty-path diagnostics |
| `just check-sdk` Go stage | PASS, Go 1.27.3, official Storage SDK v1.70.0, full Go vet/test suite against strict/permissive local HTTP servers |
| Pinned Ruby `https.rb --self-check` and HTTP suites | PASS, Ruby 4.0.9, Bundler 4.0.23, google-cloud-storage 1.62.1; strict/permissive servers initialized from `config/buckets.json`; signed GET/PUT and response overrides included |
| `just check-containers` build; `bash scripts/smoke-containers.sh --image IMAGE PLATFORM` smoke | PASS final Linux ARM64 and emulated AMD64 smoke, including read-only initializer mounts, ready counts, API mutation persistence and existing seed lifecycle; each image built once |
| `cargo xtask dependencies check` | PASS, all compiler/library/SDK/container inputs CURRENT; no dependency changes |
| Markdownlint on changed English/Japanese guides and contract | PASS, zero issues, following existing per-file rules |
| `cargo xtask compatibility render` | PASS, 591 rows validated; English/Japanese views generated from the same ledger |

The checked process cases are `initialized_settings_are_independent_and_api_changes_survive_restart`, `project_mismatch_warns_without_mutation_and_deleted_buckets_are_recreated`, `validation_reports_all_entries_before_seed_or_mutation`, `committed_initialization_survives_later_seed_and_bind_failures`, `initializer_precedes_seed_and_empty_file_is_applied`, `strict_initialization_grants_no_api_permissions_and_legacy_preserves_native_metadata` and `selected_file_only_is_loaded_and_invalid_paths_fail_without_ready`. Startup cancellation uses `shutdown_during_initializer_validation_never_publishes_or_runs_seed`. These map respectively to the delivery gate's schema/diagnostics, settings/preservation and atomic create-missing/restart requirements. Existing shared-admin tests supply batch/rollback/cancellation evidence without adapter SQL or storage changes.

The current dependency check verifies Rust 1.99.1 and rusqlite 0.41.0, with runtime image digest `sha256:3ee00291cf386ec52a552e6a858d555debd74776eb4bfa53fa3df2cd6677515e`. No benchmark, real-cloud differential test, Windows execution or instruction-level commit/kill fault injection is claimed. The existing Shared state tests cover 1000-entry/concurrent batch atomicity, injected precommit rollback and cancellation drain; the broader initializer atomicity case remains PENDING for its untested crash/disk-full boundaries. The absolute-path scan finds only the existing planning command's own patterns. Parser unreadability assertions retain their privilege-aware behavior.

The implemented application uses one bounded blocking read/validation job, followed by one `ensure_buckets` call. One case reads earlier: strict mode's built-in identities, the trusted listener's application and the test signer, hold their implicit roles in the projects of the buckets the document creates, and authentication loads during configuration, before the runtime starts. Configuration then reads the document once and parses it as one JSON value, which is step 2's syntax check; an unreadable, oversized or malformed document fails startup there with the same initializer diagnostic, before the catalog opens. The configuration keeps that text, and the initializer's job validates it from step 2's root shape on instead of reading the file again, so the implicit projects and the created buckets come from one version of a file that may change while the server starts. Cancellation during validation drops its waiter without publishing; the blocking job has no catalog access and runtime teardown joins it. Cancellation of a submitted administrative wait leaves its owned SQLite permit intact; catalog close drains it before unlocking, as verified by Shared state's existing cancellation test. Project mismatches emit safe diagnostics without project values. The source and input files are never rewritten.

Verification on 2026-11-08 for `iamPolicy`, entry records and reconcile mode, on the branch source before integration:

| Command / check | Result |
| --- | --- |
| `cargo test --locked --test it_import_seed` | PASS, 88 tests, including 26 bucket initialization process tests and the released data directory upgrades |
| `cargo test --locked --lib -- storage:: config:: bucket_init auth:: json:: grpc:: server::buckets` | PASS, 1,321 tests, including the catalog record, migration, reset and configuration tests |
| `cargo test --locked --test it_json`; `cargo test --locked --test it_xml_auth` | PASS, 135 and 258 tests, covering the PATCH and `setIamPolicy` paths that reconcile mode shares |
| `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo xtask lint` | PASS, no new production violation |

The full suite, SDK suites and container builds were not run for this change. The writes stay inside one SQLite transaction and add no file, rename or fsync step, so no new fault-injection boundary applies.

Re-verified on 2026-11-08 on the integration branch after the recorded member set, the first comparison without a record, the kept-location digest, the whole-policy public access check and the shared creation path that seed, reseed, synchronization, API insert and the initializer now use:

| Command / check | Result |
| --- | --- |
| `cargo test --locked --test it_import_seed` | PASS, 91 tests, including 29 bucket initialization process tests and the released data directory upgrades |
| `cargo test --locked --lib -- storage:: seed:: sync:: bucket_init server::admin` | PASS, 1,047 tests, including `admin_ensure_batches_preserve_identity_order_and_rollback` |
| `cargo test --locked --test it_json bucket_http`; `cargo test --locked --test it_admin` | PASS, 75 and 73 tests (1 ignored) |
| `cargo test --locked --test it_xml_auth`; `cargo test --locked --test it_sync` | PASS, 272 and 30 tests |
| `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo xtask lint` | PASS, no new production violation |

Reconciliation outside startup, file watching, lifecycle, ACL members, directory synchronization, source import, additional settings/dependencies and other tracks' internals remain outside this contract. The next independent slice is directory synchronization or source import; neither is implied by initialization delivery.
