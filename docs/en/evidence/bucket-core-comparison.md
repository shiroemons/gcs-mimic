# Bucket core reference comparison

<!-- markdownlint-configure-file {"MD013": false} -->

Executed on **2026-10-22**, macOS ARM64, against final Rust application sources
committed at `e17469279df6241779a8050250f2eeb7220f53c8`, including profile-aware
bucket paging/resource rendering and the generic profile and harness names.
Subsequent edits reconcile documentation, ledger and generated evidence only.

The [raw output](bucket-core-comparison.txt) replaces the pre-change native
baseline: **92 requests per target, 368 records**, all four targets executed,
exit 0, no skipped profile. Targets are pinned reference emulator filesystem and memory,
gcs-mimic native and gcs-mimic `legacy-emulator` compatibility profile.
HTTP error responses remain comparison data; exit 0 means the harness completed,
not that every response was identical.

## Reproducible inputs and command

```sh
: "${GCS_MIMIC_REFERENCE_REPO:?Set the repository URL from the README comparison sources}"
bash scripts/compare-reference-emulator.sh
```

`GCS_MIMIC_REFERENCE_REPO` is required and was supplied from the pinned source
repository listed under the [README comparison](../../../README.md#feature-comparison).
The harness contains no embedded repository URL and fails before downloading or
building when the variable is unset. The downloaded archive must still match
the fixed commit and SHA-256 above.

| Input | Pin |
| --- | --- |
| Reference release | `v1.56.1` |
| Reference commit | `896ece51e49eed0946089f3089f6343855c0d106` |
| Archive SHA-256 | `13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc` |
| Go | `go1.27.3`; source graph built with `-mod=readonly` |
| Local application | Rust 1.99.1, committed Cargo.lock, locked/offline build |
| Docker | Not used by this comparison; container inputs are in [final evidence](bucket-core-parity.md) |

The script downloads the exact-commit
source archive,
checks its digest, builds isolated binaries/data under `$TMPDIR`, binds loopback,
bypasses proxies, clears inherited server credentials/configuration and removes
all children and temporary data on exit. No Google bucket or metadata server is
contacted. Upstream test suites are not executed.

Source inspected at the same revision:
bucket handlers,
resources,
response wrapper,
filesystem,
memory.
Official Google sources and fetched-byte pins remain in the
[bucket contract](../slices/bucket-core-parity.md) and
[versioning/holds contract](../slices/bucket-versioning-holds.md).

## Executed results

| Case | Reference filesystem | Reference memory | Native | Compatibility profile |
| --- | --- | --- | --- | --- |
| Create by name / subsequent GET | 200 / same resource | Same | 200 / native resource | 200 / source resource |
| Omitted project | 200 | 200 | 400 required | 200, owned by configured seed project |
| Versioning true / both settings true | 500, no created bucket | 200, true | 200, true | 200, true, durable history |
| Versioning false / default hold true or false | 200 | 200 | 200 | 200 |
| Source-only names: two characters, goog/google prefix, IPv4, repeated dots, 64 undotted bytes | 200 each | 200 each | 400 each | 200 each |
| Invalid create names / missing name | 400 invalid | Same | 400 native reason | 400 invalid |
| Malformed JSON | 400 invalid, unexpected EOF | Same | 400 parseError | 400 invalid, local message |
| Duplicate create | 409 conflict | Same | 409 conflict, native message | 409 conflict, source message |
| Missing GET / DELETE / repeat DELETE | 404 Not Found | Same | 404 Bucket not found | 404 Not Found |
| Empty DELETE | 200, exact `null\n` | Same | 204, empty | 200, exact `null\n` |
| Delete/recreate | New 200 resource | Same | Same native scope | Same source scope |
| Nonempty DELETE | 412 conditionNotMet | Same | 409 conflict | 412 conditionNotMet |
| Object preservation after nonempty rejection | 200, exact 17-byte payload | Same | Same bytes | Same bytes |

Create and immediate GET normalized bodies match within **every target**.
Both false settings are present in source-profile resources; native omits false.
The source profile renders US-CENTRAL1/region/STANDARD, projectNumber `"0"`,
metageneration `"1"`, source ETag and selfLink. Settings now have real object
effects verified through HTTP and Go in [final integration evidence](bucket-core-parity.md);
this bucket corpus alone does not test those effects.

## Every remaining profile difference

The reasons below are explicit contract boundaries in
[State and remaining boundaries](../slices/bucket-core-parity.md#state-and-remaining-boundaries).
They apply against both reference backends unless stated otherwise.

| Difference visible in corpus or scoped contract | Reason |
| --- | --- |
| Millisecond UTC timestamps versus reference offset/microsecond serialization; raw Content-Length differs accordingly | Intentional local clock representation; lengths and original bytes are retained |
| Malformed/type JSON error messages use local parser wording; status/reason match | Intentional diagnostic boundary; no Go decoder message emulation |
| Versioning true succeeds durably versus filesystem backend 500/absent; memory accepts | Intentional stronger persistence matching memory capability, not fs's limitation |
| Prefix lists omit empty items, scope by project and use native pagination; omitted-project creation belongs to seed_project and is absent from a different-project list | Strict ownership and explicit listing boundaries; list entries now match the active-profile GET resource |
| `/storage/v1/b/` returns 404 instead of a reference collection alias | Unsupported trailing-slash route; no empty-name bucket exists |
| Nonempty object resources omit source ACL/projectTeam; resource id/mediaLink pin generation, include timeFinalized, and serialize timeStorageClassUpdated differently | Unsupported ACL and source object-resource emulation; intentional existing native identity/read contract |
| Object JSON uses charset/no final newline; media has x-goog-metageneration; header ordering differs | Intentional existing object adapter; header order has no HTTP semantic meaning |
| Independently allocated object generations, opaque ETags and resulting resource lengths/links differ | Intentional identity allocation; raw values remain visible, checksums and bytes match |
| Recognized unsupported protection/settings return 501, including null/empty/false except the exact empty SDK lifecycle rule list | Strict fail-closed behavior rather than the reference's accepted-but-ignored protection |
| Bounded URI/body/query framing, explicit project validation/ownership and atomic metageneration conditions | Strict input/authorization/integrity behavior retained independently of compatibility |
| Source-profile creation is limited to 666 ASCII bytes; the reference name regex has no equivalent cap | Intentional bounded lookup contract; 666 succeeds through create/get/list/delete, 667/700 fail before publication |
| History-only buckets remain nonempty; delete is atomic with mutations; holds are enforced | Intentional stronger Google-oriented integrity; reference memory checks live objects and has no hold enforcement |
| Configurable source location flags, filesystem stale sidecars, memory-process volatility are not reproduced | Unsupported source configuration or intentional durable storage boundary |

Native additionally keeps stricter Google-oriented names, required project,
US/multi-region defaults, its metadata/ETag choices and 204/409 delete responses.
These are deliberate native behavior, not profile regressions.
The complete migration surface, source object wire parity, strict provider
authentication and real-cloud behavior remain unclaimed.

## Raw format and normalization

Each `RAW` line preserves the case, relative request, exact response body as a
JSON string, numeric status and selected headers: content-type, content-length,
etag, location, x-goog-* and www-authenticate. Transport/date headers are outside
the selected corpus. JSON null and empty body remain distinct.

Only the separate comparison representation canonicalizes JSON object ordering
and insignificant whitespace, replaces nonempty ETags with `<etag>`,
RFC3339 timeCreated/updated/timeDeleted with `<timestamp>`, and that target's
loopback origin with `<origin>`. Arrays, missing fields, errors, settings,
Content-Length and object generations are never hidden. Equality flags can thus
remain false for timestamp-dependent lengths or generation allocation alone.
The first raw line declares local path-prefix and reference-product token
redaction; response bytes contain no such paths or tokens in this run.
Selected-header order is retained conservatively.

The final [Rust/shell, SDK, container and guide checks](bucket-core-parity.md)
cover the integrated source separately. No full Google-method or lossless
migration claim follows from this bounded comparison.
