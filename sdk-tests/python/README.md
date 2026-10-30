# Official Python SDK HTTP and HTTPS test

<!-- markdownlint-configure-file {"MD013": false} -->

This suite runs the official `google-cloud-storage` Python SDK against isolated
permissive and strict gcs-mimic servers over HTTP or HTTPS. It mirrors the
journeys of the [Ruby suite](../ruby/README.md) where the Python SDK exposes
them, and documents the places where it cannot. It covers the basic API
milestone and local transport, rather than full Python compatibility.

## Pins, license and maintenance

| Input | Pin | License | Verified source |
| --- | --- | --- | --- |
| Python | 3.14.10 (`.python-version`, selected by uv) | PSF-2.0 | python.org release API: newest published 3.14 release, 2026-10-25 |
| `google-cloud-storage` | 3.17.1 | Apache-2.0 | PyPI: latest, not yanked, uploaded 2026-10-15 |
| `cryptography` | 50.0.2 | Apache-2.0 OR BSD-3-Clause | PyPI: latest, not yanked, uploaded 2026-09-30 |

Both direct dependencies were checked against official PyPI metadata on
2026-10-30. The SDK is maintained by Google in the active
`googleapis/google-cloud-python` monorepo (`packages/google-cloud-storage`);
the former `googleapis/python-storage` repository is archived. `cryptography`
is maintained by the Python Cryptographic Authority (`pyca/cryptography`), and
provides google-auth's offline RSA signer for the signed URL journeys. These are
the same versions as the [SDK scorecard](../../sdk-scorecard/README.md), and
`uv.lock` resolves the same hash-pinned transitive graph (requests, urllib3,
google-auth, google-api-core, google-crc32c and their dependencies). The test
runner is the standard library `unittest`, so no test framework is added.

## Run

The repository runner starts both servers and calls `run.sh`:

```sh
bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-python
GCS_MIMIC_SDK_SCHEME=https bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-python
```

`run.sh` needs [uv](https://docs.astral.sh/uv/). It runs `uv sync --locked`,
which never re-resolves and works offline once the uv cache holds the locked
wheels, then runs four steps and prints one line per case:

1. `test_self_check`: offline origin, guard and trust-context checks.
2. `test_migration`: the anonymous migration scenario against the runner's
   preinitialized `assets` bucket on `GCS_MIMIC_ENDPOINT`.
3. `test_journeys`: every journey against the permissive `GCS_MIMIC_ENDPOINT`.
4. `test_journeys` and `test_strict`: the same journeys plus authorization
   denial and signed URLs against `GCS_MIMIC_STRICT_ENDPOINT`, with the fixture
   token `local-admin-token`.

Any failure exits nonzero. To run against servers you started yourself, export
`GCS_MIMIC_ENDPOINT` and `GCS_MIMIC_STRICT_ENDPOINT`, plus `GCS_MIMIC_CA_CERT`
for HTTPS (for example from `cargo run --locked -- tls ca`). The strict server
must use the runner's identity file, in which the fixture admin principal also
holds the public key from `fixtures/signing/test-only-public.pem`.

## Hermetic transport

- The origin must be a loopback HTTP or HTTPS origin without user info, path,
  query or fragment. A `localhost` origin is accepted only when every resolver
  answer is a loopback address.
- Every SDK request goes through one `requests` session that refuses any other
  scheme, host or port, disables environment proxies and redirects, and applies a
  ten-second timeout. Resumable session URLs must therefore keep the exact origin.
- The client uses explicit `AnonymousCredentials`, an explicit project and the
  scheme-inclusive `STORAGE_EMULATOR_HOST` / `api_endpoint`, so google-auth never
  runs default credential discovery or contacts the metadata server. Credential
  and endpoint override variables are removed from the process environment.
- In strict runs the session attaches the static fixture bearer token itself; no
  google-auth credential refreshes it. Signed and anonymous requests use a
  separate guarded session without the token.
- HTTPS trusts only `GCS_MIMIC_CA_CERT` and keeps peer and hostname
  verification with Python's default flags, including `VERIFY_X509_STRICT` on
  Python 3.13+. A live case also verifies that the public trust store rejects
  the local certificate.

## Journeys

| Journey | Python SDK API |
| --- | --- |
| Default bucket create, get, list, PATCH labels, replacement PUT, delete | `Client.create_bucket`, `get_bucket`, `list_buckets`, `Bucket.patch`, `Bucket.update` |
| PUT keeps unrelated settings (versioning, labels) while enforcing PAP | `Bucket.update` |
| Regional location `ASIA-NORTHEAST1` | `create_bucket(location=)` |
| Unsupported `includeFoldersAsPrefixes` fails fast with 400 `badRequest` and gcs-mimic diagnostics under the default retry policy | `list_blobs(include_folders_as_prefixes=True)` |
| Pub/Sub notification create, get, list, delete | `Bucket.notification`, `get_notification`, `list_notifications` |
| Bucket, default object and object ACL entries | `acl`, `default_object_acl`, `Blob.acl` |
| Autoclass with toggle time, NEARLINE terminal class and STANDARD storage | `autoclass_enabled` |
| Project service account | `get_service_account_email` |
| HMAC keys, including rejected ACTIVE delete and `showDeletedKeys`; strict admin denial | `create_hmac_key`, `list_hmac_keys`, `HMACKeyMetadata` |
| Uniform bucket-level access enable and disable | `iam_configuration` |
| Lifecycle create, replace and clear | `add_lifecycle_*_rule`, `clear_lifecycle_rules` |
| `testIamPermissions`; strict viewer and ungranted principals | `test_iam_permissions` |
| Temporary hold blocks delete; custom time on upload and update | `temporary_hold`, `custom_time` |
| Default `application/octet-stream` for an extensionless file upload | `upload_from_file` |
| 576 KiB resumable upload in three 256 KiB chunks with `ifGenerationMatch=0`, one same-origin session URL, get, list, `matchGlob`, metadata PATCH and replacement PUT, copy, rewrite, compose, CRC32C-verified downloads compared by SHA-256 | `Blob.open("wb")`, `Blob.patch`, `Blob.update`, `Bucket.copy_blob`, `Blob.rewrite`, `Blob.compose`, `download_to_filename` |
| Range downloads (closed, open and suffix) | `download_as_bytes(start=, end=)` |
| Atomic move | `Bucket.move_blob` |
| Soft-delete policy, soft-deleted listing and get, restore | `soft_delete_policy`, `list_blobs(soft_deleted=True)`, `restore_blob` |
| Retention policy blocks delete, lock on an empty bucket, locked shortening rejected | `retention_period`, `lock_retention_policy` |
| HTTPS trust and origin guard | guarded `requests` session |
| Anonymous migration scenario on the initialized `assets` bucket | `get_bucket`, `upload_from_string`, `get_blob`, `download_as_bytes` |
| Strict denial: missing token is 401, objectViewer upload is 403 and leaves no object | guarded session token |
| Strict V4 signed GET/PUT with signed Content-Type; changed/missing header, query, object, Host and method are rejected; unsigned read is 401 | `Blob.generate_signed_url(version="v4")` |
| Strict V4 response overrides and tampered override rejection | `response_disposition`, `response_type` |
| Strict V2 signed GET/PUT with the default and explicit version | `Blob.generate_signed_url()` |

Range downloads and the explicit JSON denial case go beyond the Ruby journey.
The signer is a separate offline service-account
credential built from the public test-only fixture key; google-auth never
contacts its `token_uri`.

## Known SDK limitations and adaptations

- Uploads with a known size up to 8 MiB always use one multipart request, even
  when the blob has a chunk size. The resumable journey uses the public
  `Blob.open("wb")` writer, which always streams resumable chunks.
- The SDK infers a content type only from a local filename, never from the
  object name. `upload_from_string` defaults to `text/plain` and
  `upload_from_file` to `application/octet-stream`. The migration scenario
  therefore relies on the string default rather than name-based inference.
- `Bucket.get_notification` does not parse the returned topic into
  `topic_name`; the test checks the raw resource there and the parsed fields on
  `list_notifications`, which uses `from_api_repr`.
- `Bucket.update` and `Blob.update` send the whole local resource. The
  replacement PUT cases use a fresh resource object holding only the replacement
  fields; there is no separate generated client layer like Ruby's
  `Google::Apis::StorageV1`.
- The V4 signer binds the client endpoint's `host:port` by default, so unlike
  Ruby no bucket-bound hostname adaptation is needed and URLs are used unchanged.
- `localhost` is checked through the resolver at startup instead of pinning the
  socket address as the Ruby suite does.

Official sources:

- [Python release API](https://www.python.org/api/v2/downloads/release/?is_published=true&pre_release=false)
- [Storage SDK release metadata](https://pypi.org/pypi/google-cloud-storage/json)
- [cryptography release metadata](https://pypi.org/pypi/cryptography/json)
- [Storage SDK source](https://github.com/googleapis/google-cloud-python/tree/main/packages/google-cloud-storage)
- [Storage Python client API](https://cloud.google.com/python/docs/reference/storage/latest)
