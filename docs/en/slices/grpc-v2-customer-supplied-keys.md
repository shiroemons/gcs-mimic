# Storage v2 customer-supplied encryption keys

<!-- markdownlint-configure-file {"MD013": false} -->

Scope: the gRPC part of BEHAVIOR-OBJECT-037 and the customer-supplied key fields of RPC-STORAGE-010 (ComposeObject), RPC-STORAGE-011 (DeleteObject), RPC-STORAGE-012 (RestoreObject), RPC-STORAGE-014 (GetObject), RPC-STORAGE-015 (ReadObject), RPC-STORAGE-016 (BidiReadObject), RPC-STORAGE-017 (UpdateObject), RPC-STORAGE-018 (WriteObject), RPC-STORAGE-019 (BidiWriteObject), RPC-STORAGE-021 (RewriteObject), RPC-STORAGE-022 (StartResumableWrite) and RPC-STORAGE-023 (QueryWriteStatus). The HTTP contract is [customer-supplied-keys.md](customer-supplied-keys.md); both APIs share the stored fingerprint and the key checks. Fidelity stays LOCAL_SIMULATION: object bytes are stored unencrypted on local disk.

## Contract sources

- The pinned [Storage v2 proto](https://github.com/googleapis/googleapis/blob/933dd1be8180a22bca2dc473b53d4f4297ee3b0c/google/storage/v2/storage.proto) (googleapis `933dd1be8180a22bca2dc473b53d4f4297ee3b0c`, the revision the [foundation contract](grpc-v2-foundation.md) pins; the later commit `d73dfb3c3be52245625b951b9c0bcf65163c549f` has the same file, SHA-256 `a0df6132967b97a89be248f1f24d277ee8df8f8d885db1d4af6249469ff0c1cf`): `CommonObjectRequestParams {encryption_algorithm, encryption_key_bytes, encryption_key_sha256_bytes}` (raw bytes, not Base64), `RewriteObjectRequest.copy_source_encryption_*` and `Object.customer_encryption`. The proto says nothing about which RPCs use the key, when it must be resent or which status a failure returns. Its RewriteObject comment names a `common_object_request_params.customer_encryption` field that does not exist; the destination key is the message's three fields.
- [Customer-supplied encryption keys](https://docs.cloud.google.com/storage/docs/encryption/customer-supplied-keys) and [using them](https://docs.cloud.google.com/storage/docs/encryption/using-customer-supplied-keys), checked 2026-11-15: no gRPC-specific statement.
- Cloud Storage over gRPC, recorded on 2026-11-15 as differential case `grpc-csek` (67 steps). Everything below marked *recorded* comes from it; everything marked *local* is a gcs-mimic decision tracked by gap SPEC-GRPC-CSEK.
- Go `cloud.google.com/go/storage` v1.70.0 and Rust `google-cloud-storage` 1.22.0 as observed clients: both send raw bytes, send the key on StartResumableWrite and the first write message only, and resend both rewrite keys on every RewriteObject call.

## Key fields and validation

The key travels in `common_object_request_params`; RewriteObject takes the source key from `copy_source_encryption_algorithm`, `copy_source_encryption_key_bytes` and `copy_source_encryption_key_sha256_bytes`. An empty field is absent (proto3), so a default message carries no key.

*Recorded*: Cloud Storage checks the key bytes first, then the algorithm, then the fingerprint. A request with only a fingerprint fails for its missing key, unlike the JSON API's header order. The parser is `CustomerKey::from_raw` in `src/csek.rs`.

| Condition | Error |
| --- | --- |
| Key missing or not 32 bytes (a 44-byte Base64 text is not a key) | KeyFormatInvalid |
| Algorithm missing or not exactly `AES256` | AlgorithmInvalid |
| Fingerprint missing, not 32 bytes or not the key's SHA-256 | KeySha256Invalid |
| Key does not match the object | KeyIncorrect |
| Encrypted object, no key | ResourceIsEncrypted |
| Unencrypted object, key supplied | ResourceNotEncrypted |

*Recorded*: every failure is `INVALID_ARGUMENT` with one `google.rpc.BadRequest` field violation whose description repeats the message and names no field. Four messages differ from the JSON API's: `base64` is lowercase, and the two resource messages read "The target object is (not) encrypted by a customer-supplied encryption key." The mapping is `status::customer_encryption` in `src/grpc/v2/status.rs`; every storage error carrying a key failure reaches it, so the wording holds on every RPC. Legacy v1 errors that pass through the same mapping now carry the same detail.

## Per-RPC behavior

| RPC | Key | Behavior |
| --- | --- | --- |
| GetObject | optional | *Recorded*: without a key, an encrypted object's metadata has `customer_encryption` and no `checksums`, even when the read mask names `checksums`; the right key reveals them; a wrong key or a key on an unencrypted object fails. A malformed key fails before the lookup. |
| ReadObject | required for encrypted data | *Recorded*: no key fails ResourceIsEncrypted before any data; the right key returns the data with `object_checksums` and `metadata.checksums`, for whole and ranged reads. |
| BidiReadObject | in `read_object_spec` | *Local*: as ReadObject on every open and every read-handle resume. A handle stores no key; a failed resume leaves the handle usable. `routing_token` and `read_mask` stay UNIMPLEMENTED. |
| UpdateObject | optional | *Recorded*: no key succeeds and the response hides `checksums`; the right key reveals them; a wrong key or a key on an unencrypted object fails before the update commits. |
| ListObjects | none | *Recorded*: encrypted objects list with `customer_encryption` and without `checksums`. |
| WriteObject, BidiWriteObject | first message | *Recorded*: the first message's key encrypts the object and the response shows its checksums. Later messages' keys are ignored, even a different one. Overwriting an encrypted object without a key creates an unencrypted generation. A malformed key creates nothing. |
| StartResumableWrite | request | *Recorded*: the key binds the session. `upload_id` messages may omit it, and any key they carry is ignored. |
| QueryWriteStatus | ignored | *Recorded*: any key, even a wrong one, is ignored; a finished session's resource shows `checksums` and `customer_encryption`. |
| RewriteObject | source and destination | *Recorded*: an encrypted source needs its key (none: ResourceIsEncrypted, wrong: KeyIncorrect); a source key for an unencrypted source fails ResourceNotEncrypted. The destination key alone decides the new object's encryption, so a source key alone decrypts, a destination key alone encrypts and both on the same object rotate its key. The response shows checksums. *Local*: every continuation repeats both keys, as the JSON API requires (`RewriteSession::check_customer_keys`); a refused continuation keeps the session. |
| ComposeObject | one key | *Recorded*: the key decrypts every component and encrypts the result; no key, a wrong key, an unencrypted component, or only unencrypted components with a key fail and create nothing. |
| DeleteObject | ignored | *Recorded*: deletes whatever key the request carries. The parameters are not parsed. |
| RestoreObject | rejected | *Local*: UNIMPLEMENTED with ErrorInfo, as the JSON API rejects restore key headers. |
| Appendable BidiWriteObject | rejected | Rapid storage does not support customer-supplied keys; UNIMPLEMENTED on any message. |
| MoveObject, CancelResumableWrite | no field | The proto carries no key. |

## Local deviation

*Recorded*: when a write starts without a key and a later message (a stream's second message, or an `upload_id` message of a keyless session) carries one, Cloud Storage answers `INTERNAL` "We encountered an internal error. Please try again." and publishes nothing. gcs-mimic answers ResourceNotEncrypted (`INVALID_ARGUMENT`) and publishes nothing, because `INTERNAL` would make clients retry a request that cannot succeed. A resumable session keeps its persisted bytes and can still be finished without the key.

## Design

- `src/csek.rs` holds the transport-neutral pieces: `CustomerKey` (only the SHA-256 fingerprint), `from_raw` beside the header parser, `CopyKeys`, `StoredHashes`, the `check_data` / `check_metadata` rules and `insert_response_headers`, which the JSON media read and the XML object responses share. It does not depend on the JSON adapter; the XML adapter reads its keys with `CustomerKey::from_headers` and answers `CsekError` itself.
- `src/grpc/v2/customer_key.rs` maps proto3's empty fields to absent ones, parses the request and rewrite keys and holds the reject rule for RPCs that refuse a key.
- `convert::object(record, StoredHashes)` shows an encrypted object's checksums only to a caller whose key was verified in the same catalog call that selected the record. Resumable completions and rewrite responses are stored with checksums, since they answer the caller that created the object; rows written before this slice describe unencrypted objects only, because gRPC could not create encrypted ones, so no schema change is needed.
- The catalog reads used by gRPC (`open_readable`, `get_readable`) take the key and check it after generation selection, conditions and authorization, in the same transaction, so an unauthorized caller cannot probe a key. gRPC reads never use the read cache.
- No new publish, rename or fsync step: the key fingerprint travels in existing metadata and resumable options.

## Evidence

```sh
cargo test --locked --lib
cargo xtask lint
cargo xtask differential validate
cargo xtask differential run --filter grpc-csek --reference compatibility/differential/reference/gcs.json
bash sdk-scorecard/run.sh --out "$TMPDIR/scorecard-grpc-csek" gcs-mimic
```

The Rust scorecard case `object.csek_rotation`, which rotates a key with RewriteObject over gRPC, is the client acceptance case.
