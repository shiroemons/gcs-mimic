# Official PHP SDK test suite

<!-- markdownlint-configure-file {"MD013": false} -->

This suite drives gcs-mimic through the official PHP client,
`google/cloud-storage` 2.6.1, over HTTP and HTTPS. It mirrors the
[Ruby suite](../ruby/README.md) journeys where the PHP SDK exposes them and
records where it cannot. It covers the basic API milestone and local transport,
not full PHP compatibility; the broader ledger rows stay `PARTIAL`.

## Pins and verification

| Input | Pin |
| --- | --- |
| PHP SDK | `google/cloud-storage` 2.6.1 (source `cf14f967fa45113e2a239fe9103bada8d7b92021`), exact pin in `composer.json` |
| Dependency graph | `composer.lock`, the same 26-package graph as `sdk-scorecard/php/composer.lock` (only the content hash differs) |
| Runtime | Official `composer:2.11.0` image (PHP 8.5.12), pinned by manifest index digest `sha256:278dfc7bd35169965d03f87194816f43bf4e6b6ea61707c2114e52ad12fe24ce` in `run.sh` |

PHP is not required on the host: Composer and PHP run only in that container.
The pin is identical to the one in `sdk-scorecard/run.sh`, whose digest the
daily dependency check already tracks.

Verified on 2026-10-30 from official metadata: Packagist lists 2.6.1
(2026-10-16) as the newest stable release, Apache-2.0, requiring PHP `^8.2`.
The package is Google-maintained and published from the active
`googleapis/google-cloud-php` monorepo through the
`googleapis/google-cloud-php-storage` split (not archived). The locked graph is
Apache-2.0, BSD-3-Clause and MIT only (Guzzle, google/auth, cloud-core, gax,
protobuf, Monolog, PSR and Symfony polyfills). No PHPUnit or other test
dependency is added: a small runner in `harness.php` prints one line per case
and exits nonzero on any failure.

## Running

Docker is required. From the repository root, the shared runner starts isolated
permissive and strict servers and then this suite:

```sh
bash scripts/sdk-go.sh --with-php
GCS_MIMIC_SDK_SCHEME=https bash scripts/sdk-go.sh --with-php
```

`run.sh` can also target servers you started yourself:

```sh
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 \
  GCS_MIMIC_STRICT_ENDPOINT=http://127.0.0.1:4444 \
  bash sdk-tests/php/run.sh
```

For HTTPS endpoints, also set `GCS_MIMIC_CA_CERT` to the CA file (for example,
the output of `gcs-mimic tls ca`); it is mounted read-only into the container.
The strict server must use `--auth-mode strict` with `config/identities.json`
extended by the public signing fixture for `local-admin@example.invalid`, as
`scripts/sdk-go.sh` does.

`run.sh` runs `composer install --no-interaction --no-scripts --prefer-dist`
into the gitignored `vendor/` as the invoking user, lints the PHP files, and
then runs, stopping at the first failing step:

1. `--self-check`: the origin guard, without a server.
2. `--migration-scenario` against `GCS_MIMIC_ENDPOINT` (anonymous).
3. The full suite against `GCS_MIMIC_ENDPOINT` (permissive, anonymous).
4. The full suite against `GCS_MIMIC_STRICT_ENDPOINT` with
   `GCS_MIMIC_TOKEN=local-admin-token` and `GCS_MIMIC_SIGNED_TESTS=1`.

Each step uses a named, `--rm` container that the exit trap also removes.

## Container networking

The servers bind `127.0.0.1` on the host. The suite never rewrites URLs, so
signed URLs, Host headers and resumable session Locations keep the exact
origin.

- Docker Desktop (macOS, and Windows): the container starts with
  `--add-host host.docker.internal:host-gateway`, and the guarded Guzzle client
  sets cURL `CONNECT_TO` so `127.0.0.1:<port>` dials
  `host.docker.internal:<port>`. Docker Desktop forwards that name to the
  host's loopback. TLS still verifies the original `127.0.0.1` name.
- Native Linux Docker Engine: `host-gateway` resolves to the bridge address,
  which cannot reach sockets bound only to host loopback. `run.sh` therefore
  uses `--network host` on Linux, where `127.0.0.1` in the container is the
  host's loopback and no `CONNECT_TO` is needed.

Set `GCS_MIMIC_PHP_DOCKER_NETWORK=host` or `gateway` to override the choice
(for example, Docker Desktop on Linux needs `gateway`, unless its host
networking feature is enabled). Only the macOS Docker Desktop path has been
run; the Linux path is untested.

## Hermetic clients

The permissive client uses `AnonymousCredentials`; the strict client uses a
static `FetchAuthTokenInterface` that returns the local fixture token. Neither
runs credential discovery. `projectId` and `apiEndpoint` are explicit. Every
SDK, token-handler and signed-URL request goes through one Guzzle client whose
middleware refuses anything outside the configured scheme, host and port
(including Google APIs and the metadata server), with proxies and redirects
disabled, a ten-second timeout and CA plus hostname verification for HTTPS.
`localhost` is pinned to `127.0.0.1`. The suite client disables retries; the
unsupported-call case uses the SDK's default retry policy.

## Journeys

The case IDs follow the Ruby suite. Both profiles run every journey except
where noted:

- Bucket: default and regional create, get, list, label PATCH, conditional
  PATCH with a failing `ifMetagenerationMatch`, PAP PATCH preserving versioning
  and labels, Autoclass (COLDLINE stored as STANDARD), uniform bucket-level
  access, lifecycle rules, notifications, bucket/default/object ACLs, IAM
  `testPermissions` (strict: viewer and bucket-reader tokens), soft-delete
  policy and restore, retention period, retained-delete denial, lock and the
  locked-shortening error guidance, project service account.
- HMAC keys: full lifecycle in permissive mode; 403 for `storage.admin` in
  strict mode.
- Object: extensionless upload defaulting to `application/octet-stream`; a
  576 KiB disk fixture uploaded with `ifGenerationMatch=0` as three 256 KiB
  resumable chunks under the Unicode name `dir/日本語 +%?.bin`, with the session
  Location on the emulator origin and a second create rejected with 412; get,
  list, `matchGlob`, metadata PATCH and conditional PATCH; `objects.copy`,
  `rewrite` and default `compose`; `downloadToFile` with the SDK's checksum
  validation and SHA-256 comparison; a ranged read; temporary hold with
  custom time; `objects.move`.
- `includeFoldersAsPrefixes` fails fast with 400 badRequest and gcs-mimic
  diagnostics instead of a retried or silent success.
- Strict only: anonymous 401, objectViewer denied bucket and object creation,
  and the signed-URL cases below.
- Migration scenario: the seeded `assets` bucket, upload without
  `contentType` (text/plain from `.txt`), lookup, bytes and delete.

The strict signed-URL cases use `StorageObject::signedUrl` with the public
fixture key in `fixtures/signing` through a local `ServiceAccountCredentials`
signer; signing never requests a token, and no bearer header is attached.

- V4 GET/PUT: `bucketBoundHostname` set to the endpoint's `host:port` plus the
  `scheme` option. A bucket-bound hostname drops the bucket from the path, so
  the signing object is named `<bucket>/signed.txt`; the test asserts the
  authority, path and that the signature covers the real host. It verifies
  private bytes, the signed Content-Type, and rejection
  (`SignatureDoesNotMatch`) of changed or missing Content-Type, added query,
  changed object, Host and method. Response-disposition and response-type
  overrides return the requested headers, a tampered value fails, and
  metadata is unchanged. An unsigned anonymous XML GET returns 401.
- Default V4 (no `bucketBoundHostname`): the SDK emits and signs
  `https://storage.googleapis.com/...`. The test verifies that signature
  offline and that the origin guard refuses to send it.
- V2 with the omitted (default) and explicit version: `bucketBoundHostname`
  is `<host:port>/<bucket>` on the ordinary object. Over HTTPS, GET/PUT
  succeed unchanged and changed Content-Type, method and path fail.

## Known SDK limitations

- The PHP SDK exposes only PATCH for bucket and object metadata (no
  `buckets.update`/`objects.update`). The Ruby PUT-replacement journeys are
  covered by conditional PATCH with the same assertions instead; PUT itself is
  not exercised from PHP.
- The default signer targets `storage.googleapis.com` and ignores
  `apiEndpoint`, so local signed URLs require `bucketBoundHostname`.
- The V2 signer always emits `https://`. Against an HTTP endpoint the suite
  asserts that URL and does not send it (the case says so); V2 requests run
  only in the HTTPS profile. URLs are never rewritten after signing.
- `Bucket::object()` documents a `softDeleted` option but 2.6.1 drops it; the
  suite passes `softDeleted` to `StorageObject::info()` instead.
- `Lifecycle::clearRules()` leaves an empty PHP array that the SDK sends as
  `"lifecycle": []`, which gcs-mimic rejects with guidance to use
  `{"rule": []}`. The suite clears rules with the documented array form
  `['lifecycle' => ['rule' => []]]`. Official sources do not say how Cloud
  Storage itself treats `"lifecycle": []`; the question is recorded as
  `SPEC-LIFECYCLE-ARRAY` in `compatibility/gaps.json`.
- Notification options use the SDK's snake_case names (`event_types`,
  `object_name_prefix`, `custom_attributes`, `payload_format`); camelCase
  names are silently dropped by the SDK request builder.
- Guzzle's streaming handler ignores cURL options, so raw signed requests do
  not use `stream => true`; their bodies are bounded to 64 KiB after reading.

Official sources:

- [Storage SDK Packagist metadata](https://repo.packagist.org/p2/google/cloud-storage.json)
- [Storage SDK repository](https://github.com/googleapis/google-cloud-php-storage)
- [Official Composer image](https://hub.docker.com/_/composer)
- [PHP Storage client reference](https://cloud.google.com/php/docs/reference/cloud-storage/latest)
