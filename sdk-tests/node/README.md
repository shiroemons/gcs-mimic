# Official Node.js SDK HTTP and HTTPS test

<!-- markdownlint-configure-file {"MD013": false} -->

This suite runs the official `@google-cloud/storage` client against isolated
permissive and strict gcs-mimic servers over HTTP or HTTPS. It mirrors the
journeys of the [Ruby suite](../ruby/README.md) where the Node.js SDK exposes
the same operation, and records each place where it does not. It covers the
basic API milestone and local transport, rather than full Node.js SDK
compatibility.

## Pins and dependency review

The suite pins Node.js 24 or newer (`engines`) and two exact direct npm
dependencies, verified against the official npm registry on 2026-10-30:

| Package | Version | License | Published | Notes |
| --- | --- | --- | --- | --- |
| `@google-cloud/storage` | 8.4.0 | Apache-2.0 | 2026-10-09 | `latest` dist-tag; Google-maintained (`googleapis/google-cloud-node`); 1,688,266 bytes unpacked; declares Node.js >=22 |
| `google-auth-library` | 11.3.2 | Apache-2.0 | 2026-10-29 | `latest` dist-tag; Google-maintained (`googleapis/google-cloud-node`); 784,244 bytes unpacked; declares Node.js >=22 |

`google-auth-library` was already in the SDK's resolved graph; the direct edge
exists only so the strict run can construct the documented `OAuth2Client`
holding a static local token. `package-lock.json` pins the complete graph
(59 packages) with npm integrity hashes, and its resolved package set is
identical to `sdk-scorecard/node/package-lock.json`. Installation uses
`npm ci --ignore-scripts`, so no dependency install script runs.

## Running

`scripts/sdk-go.sh --with-node` starts both servers and then runs `bash sdk-tests/node/run.sh` with
`GCS_MIMIC_ENDPOINT`, `GCS_MIMIC_STRICT_ENDPOINT` and, for HTTPS,
`GCS_MIMIC_CA_CERT`:

```sh
bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-node
GCS_MIMIC_SDK_SCHEME=https bash scripts/with-test-tmpdir.sh bash scripts/sdk-go.sh --with-node
```

To run against servers you started yourself (the permissive one seeded with
the `assets` bucket in `local-project`, the strict one with
`--auth-mode strict --identity-file` and the admin signing public key, as
`scripts/sdk-go.sh` prepares them):

```sh
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 \
  GCS_MIMIC_STRICT_ENDPOINT=http://127.0.0.1:4444 bash sdk-tests/node/run.sh
```

`run.sh` installs with `npm ci --ignore-scripts --no-audit --no-fund` (offline
when the npm cache already holds the locked packages), then runs four
`node --test` stages with the spec reporter, one line per case:

1. `self-check.test.mjs`: origin and socket-guard checks without a server.
2. `migration-scenario.test.mjs`: anonymous, against the permissive server.
3. `suite.test.mjs`: anonymous, against the permissive server.
4. `suite.test.mjs` with `GCS_MIMIC_TOKEN=local-admin-token` and
   `GCS_MIMIC_SIGNED_TESTS=1`, against the strict server.

Any failing case makes `run.sh` exit nonzero, which fails the runner.

## Hermetic transport

- The endpoint must be a bare loopback HTTP or HTTPS origin; Google hosts,
  the metadata server, other schemes, user info, paths and queries are rejected.
- `net.Socket#connect` is guarded for the whole process. Both SDK transports
  (teeny-request for JSON calls, gaxios for resumable uploads) and the raw
  signed-URL requests can only connect to the exact host, port and scheme
  (TLS or plain) of the emulator origin. Redirects, resumable session URLs and
  credential discovery cannot leave it. `localhost` is accepted as a host
  string, but dialing is not pinned to `127.0.0.1`; the runner uses `127.0.0.1`.
- Proxy variables, `STORAGE_EMULATOR_HOST`, `GOOGLE_APPLICATION_CREDENTIALS`,
  `NODE_TLS_REJECT_UNAUTHORIZED` and `NODE_OPTIONS` are unset;
  `METADATA_SERVER_DETECTION=none` is set. `STORAGE_EMULATOR_HOST` is cleared
  because the SDK treats it as a JSON API base path, not an origin; the suite
  uses `apiEndpoint`.
- The permissive client is anonymous: with a custom `apiEndpoint` the SDK
  bypasses `google-auth-library`. The strict client passes
  `authClient: OAuth2Client` holding only a non-expiring static access token
  with `useAuthWithCustomEndpoint: true`; it has no refresh path to Google.
- Signed URLs use a separate signer client with the clearly public
  `fixtures/signing/test-only-private.pem` key for
  `local-admin@example.invalid`. Signing is local; that client sends no request.
- HTTPS trusts the emulator CA through `NODE_EXTRA_CA_CERTS`, with certificate
  and hostname verification enabled. A diagnostics-channel observer checks
  that every HTTP request used the emulator scheme.

## Journeys

| Journey | Node.js SDK calls | Profiles |
| --- | --- | --- |
| Bucket PATCH preserves versioning and labels | `createBucket`, `setMetadata` | both |
| Default bucket creation and regional location | `createBucket(name)`, `createBucket(name, {location})` | both |
| Unsupported query fails fast with default retries | `getFiles({includeFoldersAsPrefixes})`: 400 badRequest under 3 s, diagnostic names gcs-mimic | both |
| Notifications | `createNotification`, `notification().getMetadata`, `getNotifications`, `delete` | both |
| JSON ACL and default object ACL | `acl.add/get/delete`, `acl.default.*`, inherited object ACL | both |
| Autoclass | `createBucket({autoclass})`, COLDLINE upload stored as STANDARD | both |
| Project service account | `getServiceAccount` | both |
| HMAC keys | `createHmacKey`, `getHmacKeys`, ACTIVE delete rejection, INACTIVE, delete, `showDeletedKeys`; strict admin is denied 403 | both |
| Uniform bucket-level access | create enabled, read `lockedTime`, disable | both |
| Lifecycle rules | create, replace, `addLifecycleRule` append, clear | both |
| testIamPermissions | admin all; strict viewer `storage.objects.get` only; bucket reader none | both |
| Strict authorization and denial | anonymous 401, unknown token 401, viewer read allowed and write/delete 403, bucket reader read 403 | strict |
| Temporary hold and custom time | upload with `customTime`, hold blocks delete (403), release, update | both |
| Bucket get/list/update with metageneration | `getMetadata`, `getBuckets`, conditional `setMetadata` (412 then success) | both |
| Default content type | extensionless `save` stores `application/octet-stream` | both |
| Resumable upload | 576 KiB disk file in three 256 KiB chunks, single same-origin Location, `ifGenerationMatch: 0` then 412 | both |
| Object get/list/matchGlob | `getMetadata`, `getFiles`, `matchGlob` hit and empty | both |
| Object metadata with metageneration | `setMetadata` PATCH, conditional replacement (412 then success) | both |
| Rewrite, compose, downloads and ranges | `copy` (rewrite), storage-class rewrite, `combine`, `download({destination})` with SDK checksum validation and SHA-256, boundary and suffix ranges | both |
| Soft delete and restore | `softDeletePolicy`, `getFiles({softDeleted})`, soft-deleted `getMetadata`, `restore`, bytes | both |
| Retention policy | `setRetentionPeriod`, retained delete 403, `removeRetentionPeriod`, `lock`, locked shortening 400 | both |
| Atomic move | `moveFileAtomic` | both |
| V2 signed GET/PUT | default and explicit `v2` URLs; signed Content-Type, changed type/method/path rejected; response overrides; anonymous 401 | strict |
| V4 canonical host | URL keeps host:port; signature verified locally against `host:<hostname>` only; strict server rejects it | strict |
| HTTPS trust and hostname | public roots alone rejected; out-of-SAN server name rejected; emulator CA accepted | HTTPS |
| Migration scenario | anonymous `assets` bucket, `save` (multipart and the default single-request resumable upload), `getMetadata`, `download`, `delete` | permissive |

## SDK limitations

These are properties of `@google-cloud/storage` 8.4.0 observed by this suite:

- **No bucket or object PUT.** `setMetadata` is always PATCH. The Ruby
  bucket-PUT and object-PUT replacement journeys are expressed as conditional
  PATCH requests that replace the same fields.
- **No `objects.copy`.** `File#copy` uses `objects.rewrite`; the generated
  copy call that Ruby exercises is not reachable.
- **V4 signing drops a nondefault port.** `signer.js` puts `URL#hostname` into
  the canonical `host` header while the emitted URL keeps `host:port`. No public
  option (`cname`, `host`, `virtualHostedStyle`) binds the port, so strict
  verification at a local nondefault port rejects every V4 URL. The suite
  proves the canonical host locally and asserts the 403
  `SignatureDoesNotMatch`; it does not claim a successful strict V4 journey.
  The V4 response-override and tamper cases that Ruby runs therefore have no
  Node.js counterpart. V2 does not sign the host and succeeds.
- **The streaming resumable path ignores `useAuthWithCustomEndpoint`.**
  `File#startResumableUpload_` (used by `save`, `createWriteStream` and
  `Bucket#upload`) does not forward the option, so at a custom endpoint it
  initiates sessions without credentials. The strict run creates the session
  with `File#createResumableUpload` (which forwards it) and passes `uri`.
- **`addLifecycleRule(rule, {append: false})` sends `append=false`** as a JSON
  API query parameter, which gcs-mimic rejects as unknown. The suite replaces
  rules with `setMetadata` and uses the default append mode.
- The signer client's `credentials` option prints a deprecation warning from
  `google-auth-library`; the replacement `JWT` client would probe the metadata
  server for `getCredentials`, so the warning is accepted.
- The SDK's internal streams emit `MaxListenersExceededWarning` during
  downloads; it does not affect results.

## Default single-request resumable upload

`File#save` and `createWriteStream` default to a single-request resumable
upload whose PUT streams the whole body with `Content-Range: bytes 0-*/*`
(unknown end and total). `node-migration-default-save` runs that default path
in the migration scenario; the other journeys select `resumable: false` or an
explicit `chunkSize` to pin the multipart or chunked transport they exercise.
The accepted open-ended range forms are described in the
[resumable upload contract](../../docs/en/slices/resumable-upload.md).

Official sources:

- [Node.js releases](https://nodejs.org/en/about/previous-releases)
- [`@google-cloud/storage` registry metadata](https://registry.npmjs.org/@google-cloud/storage)
- [`google-auth-library` registry metadata](https://registry.npmjs.org/google-auth-library)
- [Storage Node.js client source](https://github.com/googleapis/google-cloud-node/tree/main/handwritten/storage)
