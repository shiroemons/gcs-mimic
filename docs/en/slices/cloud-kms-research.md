# Cloud KMS (CMEK) research

Status: research for a future slice. Nothing here is implemented, and the [feature ledger](../../../compatibility/features.json) still owns status. Features: `BEHAVIOR-OBJECT-038`, `BEHAVIOR-BUCKET-006`, `EXT-BEH-038`.

Checked on 2026-11-09 against commit `554c7b04`; line numbers refer to that commit. The research read code and official documents. It did not run the code or contact Cloud Storage or Cloud KMS.

## Summary

- CMEK can be a `LOCAL_SIMULATION` without a key registry: validate the key format, persist the name and synthesize a key version. Bytes stay unencrypted on disk, as for customer-supplied keys.
- Keep rejecting `kmsKeyName` in a JSON object body, which Discovery defines as a 400.
- Medium to large slice, best split into JSON, gRPC and catalog; XML headers and bucket encryption configuration; and an optional key-state administration API.

## Google contract

Request locations (Discovery revision 20261103; the repository pin is 20261013 and needs reconciling first):

- JSON query: `kmsKeyName` on insert and compose, `destinationKmsKeyName` on copy and rewrite, on those four methods only.
- JSON body: `Object.kmsKeyName` is "Not currently supported. Specifying the parameter causes the request to fail with status code 400". Python's writable fields omit it and Go sends the query parameter. The reason string is unresolved.
- JSON bucket: `encryption.defaultKmsKeyName`, cleared with `null` in the curl example.
- XML: `x-goog-encryption-kms-key-name` on PUT, multipart initiation, copy and compose. The bucket default uses `?encryptionConfig` GET and PUT and `EncryptionConfiguration` at creation; an empty element clears it. Objects expose `<KmsKeyName>` from `GET ?encryption`.
- gRPC v2: `WriteObjectSpec.resource.kms_key`, `ComposeObjectRequest.kms_key`, `RewriteObjectRequest.destination_kms_key` (`destination.kms_key` "must not be populated"), `Bucket.Encryption.default_kms_key`, `Object.kms_key`.
- Conflict: the Use CMEK rewrite curl example sends `?kmsKeyName=`, while the reference and Discovery use `destinationKmsKeyName`. The native profile accepts undeclared queries, so an ignored key would be a false success. Reject `kmsKeyName` on rewrite explicitly.

Responses:

- Objects return a version-qualified name: the XML header "includes the key version", the gcloud sample shows `.../cryptoKeyVersions/VERSION_NUMBER`, and python-storage comments agree.
- A bucket default has no version.
- Python refuses to send a version-qualified input, and the server's answer is unresolved. Proposed: 400 `cloudKmsBadKey`.
- Unresolved: the XML `?encryption` example shows no version (a 2016 example that conflicts with the header text), and whether gRPC `kms_key` carries a version.

Precedence and interactions:

- Explicit key, then bucket default, then standard encryption. A customer-supplied key beats a default CMEK ("uses the customer-supplied key").
- Sending CMEK and CSEK together conflicts with "A single object can only be encrypted by one of these methods at a time", and Go errors client-side. The server status and reason are unresolved. Proposed: 400.
- Rewrite, copy and compose do not inherit the key. Without a key they use the destination default, otherwise standard encryption.
- Changing a bucket default does not affect existing objects.
- Metadata updates cannot set CMEK ("Include the key as part of a rewrite"). The mapping to `cloudKmsCannotChangeKeyName` is unresolved.
- Compose with CSEK needs one key for all components. Mixed CMEK sources and a CSEK source with a CMEK destination are unresolved.
- XML cannot rotate from CSEK to CMEK (documented); JSON can.
- Retention and holds only prevent key-version destruction, which has no local key lifecycle.
- `*EnforcementConfig` belongs to `BEHAVIOR-BUCKET-007`; keep rejecting it.
- Rapid's unsupported list includes CSEK but not CMEK, so CMEK there is unresolved. Appendable objects keep rejecting it.
- POST forms have no documented KMS field, and XML compose is not implemented locally, so both are out of scope.

Checksums, where documents conflict:

- The Objects resource says "not present for ... CMEK". The CMEK overview says "not returned when listing objects with the JSON API", and gcloud fetches them with an extra GET.
- Python `_validate_checksum` and Node `#validateIntegrity` fail when an upload response has no checksum.
- Proposed: omit hashes only from JSON list and return them from insert and get. XML `x-goog-hash` and ETag and gRPC checksums are unresolved.

Key names and errors:

- Format: `projects/*/locations/*/keyRings/{id}/cryptoKeys/{id}` with IDs matching `[a-zA-Z0-9_-]{1,63}`.
- The key ring and data location must match (dual-region uses the location code). `global` appears in Discovery and Go examples but is unresolved.
- 400 reasons (XML capitalizes the first letter): `cloudKmsBadKey`, `cloudKmsCannotChangeKeyName`, `cloudKmsDecryptionKeyNotFound`, `cloudKmsDisabledKey`, `cloudKmsEncryptionKeyNotFound`, `cloudKmsKeyLocationNotAllowed`. Documents do not map conditions to reasons, and permission-failure status is known only from third-party reports.

Local simulation proposal: no registration, only format validation, location matching and synthesized `/cryptoKeyVersions/1`. Record missing, disabled and permission behavior as known deviations. A later optional slice can add key state through an API outside Google namespaces.

Decision needed: whether location matching applies in both profiles. The recommendation is yes, which makes tests using `locations/global` return 400.

Sources (all 2026-11-09): [customer-managed keys](https://docs.cloud.google.com/storage/docs/encryption/customer-managed-keys), [using CMEK](https://docs.cloud.google.com/storage/docs/encryption/using-customer-managed-keys), [customer-supplied keys](https://docs.cloud.google.com/storage/docs/encryption/customer-supplied-keys), [enforcement](https://docs.cloud.google.com/storage/docs/encryption/enforce-encryption-types), [JSON objects](https://docs.cloud.google.com/storage/docs/json_api/v1/objects), [JSON buckets](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets), [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes), [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers), [Discovery](https://storage.googleapis.com/$discovery/rest?version=v1), [KMS key rings](https://docs.cloud.google.com/kms/docs/reference/rest/v1/projects.locations.keyRings/create), the pinned `src/grpc/proto/google/storage/v2/storage.proto`. Go 1.70.0, Ruby 1.62.1, python-storage and nodejs-storage were comparison material, not contract.

## Code touchpoints

- JSON: `src/json/objects.rs:316` (input), `:916` (output), `:532` (keep rejecting the header); `compose.rs:33`; `object_copy.rs:40`; `rewrite.rs:131`; `object_metadata.rs:98` (keep the 400, message only); `error.rs:262` and `:737` for `From<KmsError>`; `object_list.rs:163` (hash removal); `buckets.rs:14`, `:283`, `:339`, `:557`, `:963`.
- Shared: new `src/kms.rs` beside `src/csek.rs`; `src/object.rs:108`, `:302`, `:481`; `src/bucket.rs:145`, `:185`.
- Server: `objects/upload.rs:93`, `:182`; `resumable.rs:75`; `objects/derived.rs:152`, `:219`; `rewrite.rs:236` (`rewrite_token.rs:113` already binds continuation).
- Catalog: `catalog.rs:149`, `:156`; `schema.rs` steps; `objects.rs:47`, `:750`; `objects/publication.rs:43`, `:170`, `:369`; `buckets.rs:5`, `:294`, `:375`; `bucket_update.rs:4`, `:196`, `:385`; `bucket_soft_delete.rs:23`, `:93`; `resumable/options.rs:40`, `:67`, `:123`; `upload_replay/fingerprint.rs:102`; `compose.rs:210`; `appendable.rs:842`.
- XML: `mod.rs:719`, `:759`, `:830`, `:854`; `put.rs:316`, `:520`; `copy.rs:242`, `:261`, `:296`; `multipart.rs:411`; `resumable.rs:103`; `buckets.rs:158` (new `?encryptionConfig` branch); `buckets/create.rs:113`; `code.rs:1`; `src/server/xml_read.rs:132`.
- Do not add the KMS header to `is_common_object_header`: a strict DELETE would then ignore it silently. Allow it on write paths only.
- gRPC v2: `object_io/write.rs:152`; `update_copy.rs:144`, `:750` (keep the `:78` rejection); `convert.rs:223`, `:358`; `buckets.rs:276` and its field macro; `bucket_update.rs:95`; `status.rs:40`. v1 (`grpc/v1/mod.rs:715`) keeps rejecting.
- Bucket initialization: `src/bucket_init.rs:569`, `:602`, `:803`; `bucket_init/written.rs:99`, `:142`, `:174`.

## Design

- One migration: `objects.kms_key_name TEXT`, `buckets.default_kms_key_name TEXT` and `soft_deleted_buckets.default_kms_key_name TEXT`, with length checks and `kms_key_name IS NULL OR customer_key_sha256 IS NULL` on objects.
- Types in `src/kms.rs`: `KmsKeyName` (`FromStr`, `location()`), `KmsKeyVersionName` (stored value) and `KmsError` shaped like `CsekError` with `reason`, `xml_code` and `message`. `ObjectMetadataUpdate` holds `KmsKeyName`; `ObjectRecord` holds `KmsKeyVersionName`.
- Resolve the default inside the transaction at `publication.rs:43`: CSEK, explicit, default, standard. Resumable binds the explicit key at start and resolves the default at finalize; documents do not say which time applies.
- `resumable/options.rs` needs `#[serde(default)]` for existing sessions.
- Reuse `csek::render`, `insert_response_headers`, `From<…> for JsonError` and `sql::row`. Replacing the fields with an `ObjectEncryption` enum touches 54 uses and belongs in a separate refactor.

## Acceptance cases

- JSON: `KMS-JSON-01` media, multipart and resumable return a version-qualified name; `02` malformed or version-qualified is 400 with no object; `03` body field on insert, PATCH and PUT is 400; `04` CMEK with CSEK is 400; `05` default key with CSEK uses CSEK with no `kmsKeyName`; `06` explicit beats default; `07` list omits hashes, get and insert return them.
- Bucket: `KMS-BKT-01` create, PATCH, PUT, null clear and metageneration; `02` default inherited by media, multipart, resumable, copy, rewrite and compose; `03` changing the default leaves existing objects unchanged; `04` enforcement fields stay rejected; `05` location mismatch is 400.
- Derived: `KMS-DRV-01` rewrite does not inherit and uses explicit, default, standard; `02` continuation token with a different key is refused; `03` CSEK source to CMEK destination in JSON; `04` compose; `05` copy; `06` rewrite with `kmsKeyName` is 400.
- XML: `KMS-XML-01` PUT, multipart and copy; `02` GET and HEAD headers; `03` `?encryptionConfig`; `04` `?encryption` forms; `05` GET and DELETE reject the header; `06` bucket creation with a default.
- gRPC: `KMS-GRPC-01` Write, BidiWrite and resumable; `02` Rewrite and Compose; `03` `destination.kms_key` refused; `04` bucket default and update mask; `05` list and read mask.
- Durability and clients: `KMS-DUR-01` restart keeps objects, buckets and sessions; `02` migration and released data upgrade; `03` soft-delete restore, move and versions keep the key; `04` `Fault::PowerLoss` only if a new durable step is added; `KMS-SDK-01` Go and Ruby.

## Ledger and gaps

- `BEHAVIOR-OBJECT-038`: `IMPLEMENTED` as `LOCAL_SIMULATION` after all cases, `PARTIAL` before. `BEHAVIOR-BUCKET-006`: actual `LOCAL_SIMULATION`. `EXT-BEH-038` stays `EXTERNAL` and `NOT_STARTED`.
- Also update `EXT-XML-015`, `035`, `046`, the four parameter rows and `FIELD:Bucket.encryption.defaultKmsKeyName` and `FIELD:Object.kmsKeyName`. `FIELD:RelocateBucketRequest.destinationKmsKeyName` is out of scope.
- Reconcile first: `SPEC-XML-PUT-EDGES`, `SPEC-COPY-PLACEMENT`, `SPEC-REWRITE-EDGES`, `SPEC-012`. Add `SPEC-KMS-*` for the unresolved items. Confirm that the replay fingerprint includes the key under `SPEC-013`.

## Live verification limits

The Cloud KMS API was not enabled on the available test project on 2026-11-09, and enabling it would create billable keys that cannot be fully deleted. Malformed and absent key names can still be probed without a real key, which fixes error shape but not successful CMEK behavior.

## Overlap

| Location | Also changed by |
| --- | --- |
| `src/storage/catalog.rs:149`, end of `schema.rs` | folders, managed folders, lifecycle |
| `src/json/buckets.rs:14`, `src/bucket_init.rs:803`, `src/bucket.rs:145`, `src/grpc/v2/buckets.rs:276` | folders (`hierarchicalNamespace` shares these lists) |
| `src/storage/catalog/buckets.rs`, `bucket_update.rs`, `bucket_soft_delete.rs` | folders |
| `src/xml/mod.rs:830`-`854`, `put.rs:316`, `copy.rs`, `multipart.rs`, `resumable.rs:103` | XML canned ACLs |
| `src/object.rs`, `objects.rs:47`, `publication.rs:43`, `:369` | lifecycle |
| `compatibility/features.json`, generated matrices | every slice |
