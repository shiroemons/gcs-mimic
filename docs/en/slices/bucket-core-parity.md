# Bucket create, get and delete source baseline

<!-- markdownlint-configure-file {"MD013": false} -->

Contract checked on **2026-10-22**. Scope: JSON-BUCKETS-005/002/001, name-only creation, basic bucket resources, empty deletion and versioning/default event-based hold settings with real object effects. The separate object-effects contract and final integrated evidence pass for the named scope. Broad API rows remain PARTIAL. The independently executed reference corpus is recorded in `docs/en/evidence/bucket-core-comparison.txt`.

## Sources

Google's contract is [insert](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/insert), [get](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/get), [delete](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete), [Bucket](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets), [naming](https://docs.cloud.google.com/storage/docs/buckets#naming), [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) and [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1). Discovery revision `20261013`, SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`, was fetched again on the check date. The [pinned Storage v2 proto](https://github.com/googleapis/googleapis/blob/14a161a98ef045be80c12ab69d924504a756f109/google/storage/v2/storage.proto) is supplementary; JSON Discovery determines these JSON types. No real-cloud requests are used.

Comparison source: pinned reference emulator **v1.56.1**, commit `896ece51e49eed0946089f3089f6343855c0d106`, cloned below `$TMPDIR` outside the checkout. Source links below always resolve to that revision:

- bucket.go: `createBucketByPost`, `validateBucketName`, `getBucket`, `deleteBucket`.
- response.go: `bucketResponse`, `bucketVersioning`, `newBucketResponse`.
- json_response.go: `jsonToHTTPHandler`, `getStatus`, `gcsErrorReason`.
- bucket_test.go: create/validation/Attrs/delete coverage.
- backend/bucket.go, fs.go, memory.go: bucket attributes, persistence, duplicate and empty checks.

Their SHA-256 digests in that order are:

```text
73ac2fded764470f13b451d281c61089704f48e79a0234cfdf164657d443cf96
1094da79bcc93f44b60e75c94bf8ad4fc89ec67b0bfc5dbc5a26eef11cfcff02
e99ac0ac585905ad6f8fd7392fe19224aa83877f5d7d4acaf4052f5fb09435d9
c6fc48033bc3d180d19be9a7f23b41fa5783fe0a4f63e5e487b71d049f38d4ea
c3d6a664ea330eabe6b5bc81de29999240c3e4b33c8a3185cf9f087ea5268518
10ce9467ad92381b8a84627d84b4dbf6cdcbae82dcb10ae8b0f48bd3cf0b29e6
d466e797aa68c85d76d5125064782389afc944387d9156b2e49291f801c1d5b1
```

## Profiles and acceptance corpus

`compat_profile=native` is the default. `legacy-emulator` is explicit, independent of `auth_mode`, and selected through `--compat-profile`, `GCS_MIMIC_COMPAT_PROFILE` or JSON `compat_profile`, with CLI > environment > file > default. Strict authentication and transactional authorization apply identically in both profiles. The source has no project ownership model: when the compatibility profile omits `project`, gcs-mimic assigns configured `seed_project` (default `local-project`) and checks create permission for that project. An explicit project retains native ownership/validation; no registry or production project resolution is implied.

Each row names its executable test ID. Source references are the symbols above; Google columns describe the authoritative contract, while local choices stay explicit.

| Test ID | Request/state case | Native behavior | pinned reference emulator profile behavior | Google contract and source evidence |
| --- | --- | --- | --- | --- |
| `bucket_core_create_get_defaults` | `POST /storage/v1/b?project=p`, `{"name":"example-bucket"}`; GET `/storage/v1/b/example-bucket` | 200; name/id/kind, default US/multi-region/STANDARD, project number, metageneration, stable timestamps/etag/link; GET equals create | 200; source fields: US-CENTRAL1/region/STANDARD, projectNumber `"0"`, metageneration `"1"`, etag `"RVRhZw=="`, updated=timeCreated; GET equals create | Insert/get return Bucket. `newBucketResponse` uses server options; the pinned executable config default is US-CENTRAL1, as executed on both backends. Request location is ignored by `createBucketByPost`. Local clock uses milliseconds; source uses up to microseconds. |
| `bucket_core_project_and_names` | Omitted project; source-valid names such as `ab`, `goog-name`, `127.0.0.1`, `a..b`, and 64 ASCII characters | Missing project: 400 `required`; native Google-oriented naming rules reject source-only names | Missing project uses seed_project; names follow source regex `^[a-z0-9][a-z0-9._-]*[a-z0-9]$` within bounded HTTP metadata/URI limits | Google requires project and stricter global naming. `createBucketByPost` never reads project; `validateBucketName` uses only regex. Invalid alphabet, missing/nonstring name fail 400 in both; no state changes. |
| `bucket_core_invalid_requests_preserve_state` | Truncated JSON, nonobject JSON, wrong name/settings types, invalid names; duplicate creation with different settings | 400 on bad input; duplicate 409; original resource unchanged | Same state guarantees; source-compatible null/unknown/trailing input rules below; duplicate 409 source message | Discovery types are string/object/boolean. Source decoder rejects wrong scalar types, accepts null defaults and ignores unknown members. Backend HTTP precheck conflicts globally; local catalog makes duplicates atomic. |
| `bucket_core_settings_round_trip` | Omitted/false/true `versioning.enabled` and `defaultEventBasedHold`; empty versioning; create then GET | Persist booleans; true fields returned, false fields omitted (versioning omitted when disabled) | Always emit `versioning:{"enabled":bool}` and `defaultEventBasedHold:bool` | Discovery declares object/boolean types; default false is the local native JSON omission choice, not proven service omission. Source `newBucketResponse` always supplies both values. Object effects are separate. |
| `bucket_core_settings_round_trip` | Pinned high-level Go SDK sends `lifecycle:{"rule":[]}` with nonnil BucketAttrs | Accept only this exact no-rule shape on creation; actual rules and extra members still return 501 | Same bounded exception; reject other lifecycle shapes despite reference ignoring them | [Storage SDK v1.70.0 serializer](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/bucket.go), `toRawBucket` and `toRawLifecycle`; the empty rule list creates no lifecycle behavior. Existing null/empty-object/array/false rejection tests remain. |
| `bucket_core_source_input_shape` | Unknown inputs, null scalar fields, trailing JSON after a complete first value, source-ignored location/class/labels | Existing typed validation and single JSON document; unsupported recognized semantics 501 | Accept null defaults and first JSON value; ignore unknown fields and location/class/labels; recognized unsupported fields return 501 | Go `json.Decoder.Decode` in `createBucketByPost` ignores unmodeled members. The local profile deliberately rejects recognized unsupported semantics rather than implying their enforcement. |
| `bucket_core_unsupported_settings_preserve_state`, `bucket_core_strict_protection_settings_never_create_an_unprotected_bucket` | Recognized unsupported settings, including enforced PAP, UBLA, retention, lifecycle rules, ACLs and encryption; subsequent policy grant/upload/anonymous read | 501 before bucket publication; failed requests cannot leave an unprotected bucket or grant | Same rejection in both authentication modes | Intentional difference: reference creation ignores these fields. Local strict reproduction proves rejected PAP cannot be followed by a successful allUsers policy grant or upload; subsequent anonymous media access remains denied. |
| `bucket_core_missing_and_delete_recreate` | Missing GET/DELETE, empty DELETE, repeat DELETE, GET after delete, recreate with defaults | Missing/repeat 404; success empty 204; recreate resets settings | Missing/repeat 404 with source message `Not Found`; success 200 application/json, exact bytes `null\n`; recreate resets settings | Delete reference requires empty bucket and empty successful response; `deleteBucket` returns `jsonResponse{}` and encoder emits null newline; missing returns status 404. |
| `bucket_core_nonempty_delete_preserves_objects` | Two uploaded objects then bucket DELETE; read each object afterward | 409; no bucket/object mutation | 412 `conditionNotMet`, source backend message; no bucket/object mutation | Google delete fails for live/noncurrent objects. Both source backends call ListObjects before delete and return BucketNotEmpty; source adapter maps it to 412. Native reason is existing local `conflict`. |
| `bucket_core_strict_permissions` | Settings creation, GET and DELETE with valid admin, read-only, wrong-project, anonymous and invalid tokens | create/get/delete permissions checked before reading body and again transactionally where applicable; denied state unchanged | Identical checks, including fallback seed_project; never use source permissive auth to bypass them | Google requires storage.buckets.create/get/delete. Local role snapshot and SPEC-017 own precise authentication/error order. |
| `bucket_core_restart_settings` | Create with true/false settings, stop/start, GET; delete/recreate then restart | Resource/settings survive; removed buckets stay absent | Same durable guarantees; source rendering preserved | Memory backend does not survive process exit; fs persists default hold sidecar but rejects versioning=true. Local durable versioning is an intentional extension over fs, matching memory configuration. |
| `bucket_core_migrates_v7_settings` | Start HTTP server from v7 catalog, GET prior bucket and stop | Same resource with false settings, catalog advances to v8 | Same durable state with source field rendering | Local migration and HTTP startup contract; prior identity/timestamps remain unchanged. |
| `bucket_core_profile_lookup_names_and_json_framing` | Create with explicit EU location, GET/DELETE and errors; source-invalid lookup names `a`, `Uppercase`, `-bad`, `bad-`, `bad!`; IAM lookups | Honor supported request location; native JSON content type includes charset, no newline; invalid names return 400 | Ignore request location and render US-CENTRAL1/region; JSON success/error content type application/json and exactly one final newline; invalid absent lookup names return 404 Not Found | `createBucketByPost` ignores location, `getBucket`/`deleteBucket` do not run create-name validation, and `jsonToHTTPHandler` uses Go JSON Encoder. Raw name segments remain capped at 666 bytes on bucket and IAM paths; malformed URI decoding still fails. |
| `bucket_core_native_restart_preserves_source_only_names_without_exposing_them` | Create source-only names, restart native, attempt create/get/delete/list, restart source profile | Reject create/get/delete with 400; list exposes retained names; no data removed | Existing buckets become reachable again and can be deleted | Intentional profile carry-over constraint: native create and lookup naming remain unchanged. Keep the source profile to access or remove source-only names; changing profiles does not convert or delete catalog data. |
| `bucket_settings_migrate_supported_catalogs`, `v1_migration_is_transactional_and_current_schema_reopens_idempotently`, `metadata_update_uses_clock_preserves_blob_and_migrates_previous_catalog` | Open older catalog, inspect false defaults, create with settings, restart | Schema v8 adds only two constrained bucket columns, default false; old metadata preserved | Same catalog | Local persistence contract; versions 1 and 3 have explicit false-default assertions; versions 4/5/6/7 cover every setting combination. Unreleased v2 remains rejected because lost generation history cannot be recovered. |
| `rejects_future_schema_unversioned_data_and_corruption`, `bucket_settings_migration_rolls_back_columns_and_version_after_late_failure` | Missing or altered column CHECK/default; forced second-column migration failure | Reject altered inventory before writing; late migration failure leaves version 7 and both new columns absent | Same catalog | Local integrity contract; failed-open fixtures compare unchanged database bytes, and a connection-local SQLite column limit forces the second ALTER to fail. |
| `compat_profile_precedence_and_validation` | CLI/environment/file/default and invalid profile strings/types | Default native; exact enum values only | Explicit profile required | Local configuration contract, independent from Google and source flags. |
| `bucket_profile_names_support_object_and_xml_routes` | Media and resumable upload, metadata/media GET, list, delete, XML GET/HEAD in source-only names; native restart rejects routes without data loss | Native naming remains unchanged on every route | Source-valid names work across bucket/object/session/XML routes | Uses bucket API creation and HTTP only; no catalog seeding. |
| `bucket_profile_paging_preserves_names_and_resource_defaults` | Size-one pages across source-only names, then native restart/carry-over pages; profile items equal GET | Retained source names remain listable with usable tokens | Listing renders source defaults consistently with GET | Project scope, empty-item omission and token encoding retain native semantics. |
| `bucket_profile_name_bound_keeps_created_buckets_reachable` | 666-byte ASCII name create/get/list/delete; 667/700-byte create rejected without publication | Native naming limits unchanged | 666-byte cap aligns create with bounded lookup segments | Intentional bounded-profile deviation from the source regex's unbounded length. |
| `bucket_api_settings_drive_versioning_and_hold_effects` | Create true settings, inherit hold, reject overwrite/delete, release, retain/read/list history, reject history-only bucket deletion, permanently delete generations and bucket | All effects pass | Same effects; bucket deletion retains profile status/body | Settings originate in HTTP bucket creation; source memory history and stronger Google hold effects are both preserved. |
| `TestBucketSettingsRoundTrip` | Official Go BucketAttrs Create+Attrs for versioning; official JSON Buckets.Insert for enabled default hold, then high-level Attrs reading both settings | Both booleans round trip | Same SDK types under source profile | Pinned official SDK harness; configuration/results only. The high-level create serializer does not send default hold, so hold=true uses the official raw JSON client without intercepting requests. |

## State and remaining boundaries

The compatibility profile rejects every member of the native unsupported-field
inventory: `acl`, `autoclass`, `billing`, `cors`, `customPlacementConfig`,
`defaultObjectAcl`, `encryption`, `hierarchicalNamespace`, `iamConfiguration`,
`ipFilter`, `lifecycle`, `logging`, `objectRetention`, `retentionPolicy`, `rpo`,
`satisfiesPZI`, `satisfiesPZS`, `softDeletePolicy` and `website`.
Each is an explicit profile-versus-reference difference: the pinned creation
struct ignores it, but returning success locally could imply protection, data
placement, billing or other behavior that is unavailable. Rejection applies even
to null/empty/false values. The sole accepted no-effect exception is the exact
SDK shape `lifecycle:{"rule":[]}`; nonempty rules and extra members remain 501.
Unknown fields and the source-ignored basic location/class/labels fields keep
their documented behavior. Versioning/default hold remain supported settings;
their object effects are implemented by the separately owned
[versioning and holds slice](bucket-versioning-holds.md).

The source profile uses the reference executable's US-CENTRAL1 region default,
ignoring per-request location like its creation struct. Configurable reference
location options remain outside this scoped profile. Switching an existing
catalog to native does not convert source-only bucket names: native GET/DELETE
reject them, while native listing still exposes them. Switch back to the source
profile to access/delete them. This restriction is tested without data loss;
native creation retains its Google-oriented name validation.

Creation publishes the bucket, default policy and incarnation in one transaction. Schema v8 stores `versioning_enabled` and `default_event_based_hold` as NOT NULL integer booleans with CHECK constraints and false defaults; a transaction-based accessor lets object mutations read the same snapshot. It does not change object tables. Invalid requests, failed permissions, duplicates and nonempty deletion must leave all prior metadata and bytes unchanged. Delete/recreate must not preserve settings or grants from the old bucket incarnation.

The filesystem source rejects enabled versioning with a 500 error and always returns false; the memory source supports it. The profile targets the source memory capability for versioning while retaining durable native persistence; it does not imitate fs's unsupported versioning or stale bucket sidecar left by DeleteBucket. Source memory deletion checks only live objects and has a check/delete race; gcs-mimic retains atomic deletion and protects all catalog generations. Those differences remain visible rather than weakening integrity.

The comparison's object-preservation probes intentionally retain the existing
native object contract in both profiles: no synthetic source ACL, generation in
resource `id` and mediaLink query, `timeFinalized`, UTC millisecond
`timeStorageClassUpdated`, native JSON charset/no-newline framing and
`x-goog-metageneration` on media reads. Independently allocated generations,
opaque ETags, timestamp precision and resulting Content-Length differ; selected
header order also differs without HTTP significance. ACL/owner emulation and
complete source object wire rendering remain unsupported separate work. The
bucket gate requires preserved bytes and usable source bucket names, rather
than silently claiming complete object-resource parity from these probes.

Native basic resource shape remains the existing Google-oriented subset: full/noAcl omit unmodeled ACL/owner, native etags/project resolution are local, and US/EU/ASIA are the only supported locations. The source profile applies its fixed resource defaults and ignores source-ignored creation fields, without adding unrelated bucket extensions. Local millisecond timestamp precision, native unknown-field/default-omission rules and stricter bounded query/body framing are explicit differences; they do not prove whole-method Google parity. Parse/type rejection statuses and source reason `invalid` match, but malformed-JSON/type error message text remains local rather than Go decoder text. Explicit project queries retain local validation/ownership although the source ignores them. Bucket list entries use the active profile and match GET; project scoping, empty-item omission and keyset paging remain native. Source-profile creation is capped at 666 ASCII bytes so every created name fits bounded lookup segments; the reference regex has no equivalent cap. A trailing collection slash (`/storage/v1/b/`) remains an unsupported route with 404 rather than the reference collection alias; it cannot create an empty-name bucket. Object, resumable session and XML routes now apply profile-aware naming. Metageneration conditions remain enforced in both profiles even though the source ignores them. Existing gaps, including SPEC-017 authentication wire details, remain open. No lossless migration, whole-method completion or real-cloud behavior is claimed. Final settings effects and integration checks are recorded in [delivery evidence](../evidence/bucket-core-parity.md).

## Verification

Verified on 2026-10-22 with pinned Rust 1.99.1 and unchanged dependency locks.
`cargo xtask dependencies check` verified every compiler, direct dependency,
Action, SDK and container input as CURRENT at slice start. No dependency was added.

| Command | Result |
| --- | --- |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --workspace` | PASS, 360 tests; no skipped tests |
| `env -u RUSTUP_TOOLCHAIN cargo test --locked --test bucket_http` | PASS, 20 tests; profile cases cover both modes, protection rejection, framing, lookup names and profile-switch persistence |
| `env -u RUSTUP_TOOLCHAIN cargo fmt --all --check` | PASS |
| `env -u RUSTUP_TOOLCHAIN cargo clippy --locked --all-targets -- -D warnings` | PASS |
| `GCS_MIMIC_GO_TEST_RUN=TestBucket bash scripts/sdk-go.sh` | PASS; bucket lifecycle, settings and permissive IAM tests, plus Go vet |
| `GCS_MIMIC_COMPAT_PROFILE=legacy-emulator GCS_MIMIC_GO_TEST_RUN=TestBucket bash scripts/sdk-go.sh` | PASS; same narrow SDK corpus and Go vet under the source profile |
| `npx --yes markdownlint-cli2 --config <MD013-disabled-config> docs/en/slices/bucket-core-parity.md docs/en/slices/json-bucket-crud.md docs/en/getting-started.md` | PASS; only MD013 disabled to preserve existing contract tables/long prose lines |

The [integrated SDK evidence](../evidence/bucket-core-parity-sdk-go.txt) retains the final raw outputs
with only local path prefixes replaced. SDK versions are Go 1.27.3, Storage
v1.70.0 and JSON API v0.307.0. Storage v1.70.0
[`BucketAttrs.toRawBucket`](https://github.com/googleapis/google-cloud-go/blob/storage/v1.70.0/storage/bucket.go#L944-L1001)
does not assign `DefaultEventBasedHold`, even when the input attribute is true.
Consequently a true high-level Create+Attrs hold round trip is blocked upstream;
the test uses official JSON `Buckets.Insert` for enabled hold and leaves the
high-level Attrs assertions for both booleans intact. The same serializer always
sends an empty lifecycle rule array for nonnil BucketAttrs, which the native
adapter now accepts as the exact no-rule shape with negative rule tests retained.
Container builds, full guide journeys, complete SDK suites, real-cloud tests and
object-side settings effects were not run in this slice; integration owns those
selected final-source checks and the ledger updates.

Every test ID in the acceptance corpus above passed for its documented scope.
The HTTP profile tests each run both profiles, including migration/restart and
strict permission denials. The recorded remaining differences are timestamp
precision, local malformed/type error message text, explicit project validation,
native list scoping/paging and the unsupported trailing collection alias.
The SDK hold creation limitation is covered by the approved official raw JSON
insert plus high-level read split; the unsupported high-level hold write is not
claimed to pass.
