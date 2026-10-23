# Object copy and derived writes

<!-- markdownlint-configure-file {"MD013": false} -->

## Scope and contract

Checked on 2026-10-23 against the official [copy](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/copy), [rewrite](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/rewrite), [compose](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/compose), [object resource](https://docs.cloud.google.com/storage/docs/json_api/v1/objects), [preconditions](https://docs.cloud.google.com/storage/docs/request-preconditions), [validation](https://docs.cloud.google.com/storage/docs/data-validation), [composite objects](https://docs.cloud.google.com/storage/docs/composite-objects) and [status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes) contracts.

This slice extends the frozen [basic JSON profile](basic-json-contract.md). It owns JSON-OBJECTS-003 and BEHAVIOR-OBJECT-023. Rewrite continuation and composite-object metadata belong to their own slices.

Raw sources were fetched again on 2026-10-23. Discovery revision `20261013` retains SHA-256 `1f74b7a93b3d2872cc332fc20dafae1657fc255930ce336618bea3d0b4d95ae0`. The latest file-changing googleapis commit remains `d73dfb3c3be52245625b951b9c0bcf65163c549f`; [storage.proto at that commit](https://github.com/googleapis/googleapis/blob/d73dfb3c3be52245625b951b9c0bcf65163c549f/google/storage/v2/storage.proto) retains SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`. The official stable Rust manifest dated 2026-10-15 reports `1.99.1 (58cf8f9c3 2026-10-13)`, matching the repository pin. No dependency is added. Serialized MD5 state uses the pinned md-5 0.11.0 format; changing that format requires a catalog migration.

The shared foundation pins each selected generation under catalog serialization by linking its immutable opaque blob into staging. Transfers own up to 32 ordered ranges and persist an output offset; an interrupted write truncates to that offset before resuming. Source deletion, overwrite and catalog restart do not change pinned bytes. Each call uses a 64 KiB buffer and shares the eight-upload admission limit. At most 128 transfers may remain active. The caller owns continuation-token expiry and cancellation.

Publication syncs the staged file, renames it into the blob directory, then commits the new object and removes the transfer in one catalog transaction. Destination conditions, effective create/delete permissions, live event holds, versioning and bucket defaults are checked in that transaction. Failure cleans staging and probes committed references before reclaiming a blob. Recovery restores a renamed but uncommitted output to staging; committed objects retain their blob and abandoned source links are removed by the normal staging scan.

## Acceptance cases

| Area | Declared behavior and checks |
| --- | --- |
| Source resolution | Live default or explicit positive source generation; all four source conditions against the selected metadata; failed conditions return 412; missing bucket/object return 404. Stable handles preserve selected bytes across concurrent source overwrite/deletion. |
| Streaming | Ordered whole objects or validated byte ranges; empty sources and range boundaries; stop exactly at a byte budget; persisted resume after restart; bounded buffers; incremental MD5/CRC32C, checked before publication. |
| Publication | New monotonic generation and metageneration 1; all four destination conditions, including generation match/not-match 0; failed publication preserves the destination and cleans transfer files. Two concurrent create-if-absent operations have exactly one winner. |
| State interactions | Same-object and cross-bucket writes; destination versioning retains old revisions; event-held destination cannot be replaced; destination default holds apply; source holds do not prevent reading. |
| Authorization | Source storage.objects.get; destination storage.objects.create; destination storage.objects.delete for replacement; effective policy rechecks within source selection and destination publication. |
| Copy adapter | POST encoded source/copyTo/destination path; sourceGeneration and eight conditions; metadata inheritance and supplied overrides; fields; noAcl response projection; 200 object resource; source bytes are unchanged. |
| Unsupported copy inputs | ACLs/full projection, encryption/KMS, requester billing, contexts, retention, temporary holds, advanced object fields and semantic headers are explicitly rejected; no false success. Unknown/output-only metadata follows the frozen profile's explicit boundary. |
| Provider boundaries | Cross-location/class large-object timeout/413 thresholds and exact error precedence/text require provider evidence. Go high-level CopierFrom uses rewrite; generated JSON Objects.Copy provides this slice's SDK operation. |

## Verification status

Implementation and final evidence are pending. Do not interpret the acceptance inventory as executed tests or full provider parity.
