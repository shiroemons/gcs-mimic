# Customer-supplied encryption keys

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

Scope: BEHAVIOR-OBJECT-037. EXT-XML-015 (`GET /OBJECT?encryption`) remains
NOT_STARTED. The row is PARTIAL with LOCAL_SIMULATION fidelity: key handling is
enforced, but object bytes are stored unencrypted on local disk.

## Contract sources

Checked 2026-10-29:

- [Customer-supplied encryption keys](https://docs.cloud.google.com/storage/docs/encryption/customer-supplied-keys)
  and [using them](https://docs.cloud.google.com/storage/docs/encryption/using-customer-supplied-keys):
  headers, metadata without a key, hashes withheld from keyless metadata and
  listings, rewrite key rotation, compose with one shared key.
- [JSON status codes](https://docs.cloud.google.com/storage/docs/json_api/v1/status-codes)
  and [XML status codes](https://docs.cloud.google.com/storage/docs/xml-api/reference-status):
  the six 400 reasons below.
- [XML headers](https://docs.cloud.google.com/storage/docs/xml-api/reference-headers):
  `x-goog-encryption-algorithm` and `x-goog-encryption-key-sha256` are also
  response headers.
- Discovery `Object.customerEncryption {encryptionAlgorithm, keySha256}` and the
  Storage v2 `CustomerEncryption` message.

## Request headers

`x-goog-encryption-algorithm: AES256`, `x-goog-encryption-key` (standard
Base64 of 32 bytes) and `x-goog-encryption-key-sha256` (standard Base64 of the
key's SHA-256) identify the object's key. Copy and rewrite read the source key
from the same names with the `x-goog-copy-source-encryption-` prefix.

| Condition | JSON reason (400) | XML code (400) |
| --- | --- | --- |
| Algorithm missing or not `AES256` | `customerEncryptionAlgorithmIsInvalid` | `CustomerEncryptionAlgorithmIsInvalid` |
| Key missing, not canonical Base64 or not 32 bytes | `customerEncryptionKeyFormatIsInvalid` | `CustomerEncryptionKeyFormatIsInvalid` |
| Fingerprint missing, malformed or not the key's SHA-256 | `customerEncryptionKeySha256IsInvalid` | `CustomerEncryptionKeySha256IsInvalid` |
| Key does not match the object | `customerEncryptionKeyIsIncorrect` | `CustomerEncryptionKeyIsIncorrect` |
| Encrypted object, no key | `resourceIsEncryptedWithCustomerEncryptionKey` | `ResourceIsEncryptedWithCustomerEncryptionKey` |
| Unencrypted object, key supplied | `resourceNotEncryptedWithCustomerEncryptionKey` | `ResourceNotEncryptedWithCustomerEncryptionKey` |

A repeated key header is invalid. Header validation precedes any body read, so
an invalid key never creates an object.

## Acceptance cases

- CSEK-HEADERS: the table above, shared by every keyed route.
- CSEK-UPLOAD: JSON media, multipart and resumable uploads and XML PUT store
  the key fingerprint. Resumable initiation binds the key; a chunk may repeat
  it, a different key or a key on an unencrypted session is rejected without
  invalidating the session, and chunks without key headers use the bound key.
- CSEK-RESOURCE: encrypted objects render `customerEncryption`. `md5Hash` and
  `crc32c` are returned only when the request supplied the matching key
  (upload, metadata GET, PATCH/PUT, copy/rewrite/compose destination); keyless
  metadata GET, listings and resumable status omit them. JSON and XML ETags of
  encrypted objects do not embed the MD5.
- CSEK-READ: JSON `alt=media` and XML GET require the matching key and return
  the encryption response headers. JSON metadata GET and XML HEAD succeed
  without a key (HEAD then omits `x-goog-hash`); a supplied key must match.
- CSEK-UPDATE: PATCH/PUT need no key; a supplied wrong key fails before the
  metadata change commits. `customerEncryption` in a PUT body must equal the
  stored value.
- CSEK-COPY: copy and rewrite require the source key for an encrypted source;
  the destination is encrypted with the request key or stored unencrypted
  without one. Rewriting an object onto itself with a new destination key
  rotates its key. Rewrite continuations must repeat both keys.
- CSEK-COMPOSE: every component must be readable with the request key, which
  also encrypts the result; mixed or keyless components are rejected.
- CSEK-REJECT: list, delete, restore and move reject key headers as unsupported
  instead of ignoring them, as do gRPC `common_object_request_params`.
- CSEK-GRPC: Storage v2 and legacy v1 gRPC cannot supply a key, so their reads,
  copies, rewrites and composes of encrypted objects fail with the
  `resourceIsEncryptedWithCustomerEncryptionKey` reason; v2 metadata reports
  `customer_encryption` without checksums.
- CSEK-PERSIST: catalog schema 30 adds a nullable 32-byte
  `customer_key_sha256` column. Existing objects migrate as unencrypted, and
  the fingerprint survives restart.
- CSEK-SDK: the SDK scorecard `object.customer_supplied_key` case passes for Go
  `cloud.google.com/go/storage` v1.70.0 (JSON upload, XML read) and Ruby
  `google-cloud-storage` 1.62.1 (JSON upload and media read).

## Local simulation boundary

Cloud Storage stores only the key's SHA-256 and encrypts the data with the key.
gcs-mimic likewise never persists, logs or returns the key itself, but it
stores object bytes unencrypted under the data directory, including the
visible tree of directory synchronization. Possession of the data directory
therefore reveals encrypted objects' contents. This keeps streaming,
range reads, compose and rewrite unchanged and avoids a new cryptographic
dependency; the SHA-256 comes from the existing `ring` dependency.

## Local choices and pending work

- XML HEAD without a key is treated as a metadata read. The HEAD reference
  does not document this case; provider evidence is pending.
- Chunks of a resumable upload may omit the key; the official resumable guide
  does not state whether each chunk must repeat it.
- A metadata GET or PATCH that supplies a key for an unencrypted object fails
  with `resourceNotEncryptedWithCustomerEncryptionKey`.
- A cached resumable completion response includes hashes for later status
  checks.
- Pending: `GET ?encryption` (EXT-XML-015), customer keys on gRPC requests,
  TLS-only key acceptance (keys are accepted over plain HTTP for local use),
  CMEK interaction, bucket encryption enforcement configuration, signed URL
  key headers and provider differential evidence.

## Evidence

```sh
cargo test --locked --lib csek
cargo test --locked --lib customer_keys
bash scripts/test-rust.sh
bash sdk-scorecard/run.sh --out "$TMPDIR/scorecard-csek" gcs-mimic
```

The scorecard run records `object.customer_supplied_key` as `pass` for both
SDKs; the remaining cases match the 2026-10-29 baseline.
