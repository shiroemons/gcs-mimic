# Official Go JSON SDK tests

This module pins Go 1.27.3, `cloud.google.com/go/storage` v1.70.0 and
`google.golang.org/api` v0.307.0, verified on 2026-10-22. `go.sum` records module
checksums; `go.mod` records the resolved graph. Go 1.21 or newer can download the
pinned toolchain with `GOTOOLCHAIN=auto`. The `go 1.27.0` directive selects the
language baseline and `toolchain go1.27.3` pins the verified stable patch release.
Using the same patch in both directives causes Go to remove the toolchain pin.

Run endpoint validation and compile the integration test without a server:

```sh
cd sdk-tests/go
GOTOOLCHAIN=auto go vet ./...
GOTOOLCHAIN=auto go test -run XXX ./...
GOTOOLCHAIN=auto go test -v ./...
```

Integration tests skip with an explicit message when `GCS_MIMIC_ENDPOINT` is
unset. `-run XXX` compiles every package without running tests. To build and start
an isolated server, run from the repository root:

```sh
bash scripts/sdk-go.sh
```

To select tests, pass a Go `-run` regular expression with
`GCS_MIMIC_GO_TEST_RUN`. Vet still checks all packages. For example, the bucket
suite can run independently:

```sh
GCS_MIMIC_GO_TEST_RUN='TestEmulatorEndpoint|TestBucketLifecycle' bash scripts/sdk-go.sh
```

The runner requires Bash, Rust, Go and jq. It builds with the pinned Rust toolchain
and starts two isolated servers, each with its own loopback port and data directory:
a permissive server for `GCS_MIMIC_ENDPOINT` and a strict server
(`--auth-mode strict --identity-file config/identities.json`) for
`GCS_MIMIC_STRICT_ENDPOINT`. For each server it checks the bound port in the ready
JSON log (the strict server must also report `"auth_mode":"strict"`), probes
`/_internal/ready`, then runs vet and uncached verbose tests. The server must
support `serve --listen IP:PORT --data-dir PATH` and the strict options. A trap
stops both servers, propagates a server failure status, reports both failure logs
and deletes only this run's temporary directory.

For an already running emulator:

```sh
GCS_MIMIC_ENDPOINT=http://127.0.0.1:4443 go test -count=1 -v ./...
```

Only loopback IPs and `localhost` are accepted by default. For a dedicated emulator
in a container network, opt in to its exact hostname using
`GCS_MIMIC_ALLOWED_HOSTS=gcs` (comma-separated hostnames, without ports). Never
allow a real Cloud Storage endpoint. The suite creates randomly named buckets
under unique test projects and deletes them and their objects, including cleanup
on test failure; use an isolated emulator.

The test sets the documented `STORAGE_EMULATOR_HOST` to the validated origin.
Storage v1.70.0 accepts a full origin or a host:port value and adds `/storage/v1/`.
`storage.NewClient` selects HTTP, `storage.WithJSONReads` selects JSON reads, and
`option.WithoutAuthentication` prevents credential discovery. Client metrics are
disabled. The HTTP transport disables proxies, rejects other request origins,
pins `localhost` dialing to loopback, and refuses redirects, preventing fallback
to Google or the metadata server. Each request and the whole test have deadlines.

Bucket coverage: create with project, get identity/metageneration/creation time,
project-scoped listing with a small page size, prefix filtering across four
explicit one-item pages, duplicate create as `googleapi.Error` 409, delete,
get-after-delete as `storage.ErrBucketNotExist`, and missing delete as
`googleapi.Error` 404. SDK version and transport appear in verbose test output.

Object coverage: raw `net/http` media upload with `ifGenerationMatch=0`, then
official SDK metadata (name, bucket, size, content type, generation,
metageneration, CRC32C and MD5), exact binary JSON reads, generation-specific
reads and delete; a nonexistent generation returns `storage.ErrObjectNotExist`.
Matching generation/metageneration conditions succeed;
incorrect `GenerationMatch`, `DoesNotExist` and `MetagenerationMatch` conditions
fail with `googleapi.Error` 412 on metadata, reads and delete, leaving bytes
unchanged. Nonempty bucket deletion fails with 409, missing metadata/reads map to
`storage.ErrObjectNotExist`, and the empty bucket can be deleted. Names include
slashes, a literal `%2F`, spaces, Unicode and reserved URI characters.

The pinned SDK's `NewReader` defaults to XML, so the shared client explicitly uses
`storage.WithJSONReads()`. Its Writer uses `Objects.Insert(...).Media(...)`:
single-request uploads are multipart and chunked uploads can be resumable; neither
is a media upload. Raw media coverage uses the same guarded HTTP client directly.
`TestObjectMultipartWriter` uses `ChunkSize=0` to exercise single-request multipart,
including empty and multichunk binary data, fixed/custom metadata,
explicit CRC32C/MD5, rejected checksum mismatches, create-if-absent and
conditioned overwrites, followed by Attrs and NewReader verification. The media
lifecycle test retains the unsupported resumable case by writing more than one
256 KiB chunk with nonzero `ChunkSize`; it requires 501 `notImplemented` with a
nonempty message and no object publication. That status is the emulator's local
unsupported-semantics deviation, not Google's resumable behavior. This harness
establishes the documented single-request Writer subset only.
Upload invocation tokens require `ifGenerationMatch` and have no replay support;
retrying a committed conditioned upload fails 412 instead of writing again.

## Strict authentication suite

`strict_auth_test.go` runs only when `GCS_MIMIC_STRICT_ENDPOINT` names an emulator
started in strict mode; otherwise each test skips with an explicit message.
`scripts/sdk-go.sh` always sets it, so the runner never skips this suite. The
tests use the local fixture registry `config/identities.json`:

| Principal | Token | Role on `local-project` |
| --- | --- | --- |
| `local-admin@example.invalid` | `local-admin-token` | `roles/storage.admin` |
| `local-viewer@example.invalid` | `local-viewer-token` | `roles/storage.objectViewer` |
| `local-creator@example.invalid` | `local-creator-token` | `roles/storage.objectCreator` |
| `local-bucket-reader@example.invalid` | `local-bucket-reader-token` | None; IAM tests grant bucket-only access |

These identities and tokens are deliberately public, non-secret emulator
fixtures. They are not Google credentials, grant nothing outside the emulator and
must never be reused for a real service. The registry schema rejects unknown keys,
so the file itself carries no comment; this README and `compose.strict.yaml`
label it instead. To run the container with the same registry:

```sh
docker compose -f compose.yaml -f compose.strict.yaml up
```

The strict clients set the endpoint explicitly with `option.WithEndpoint` and
clear `STORAGE_EMULATOR_HOST`, because the pinned SDK forces
`option.WithoutAuthentication` whenever that variable is set and would drop the
token. Anonymous clients use `option.WithoutAuthentication`. Authenticated clients
pass `option.WithTokenSource` with an `oauth2.StaticTokenSource`, which prevents
Application Default Credentials and metadata-server discovery. Because the SDK
ignores credential options when `option.WithHTTPClient` is supplied, the same
static token is attached by an `oauth2.Transport` wrapped around the guarded
emulator transport described above. `golang.org/x/oauth2` v0.37.0 was already in
the resolved graph; importing it directly only moves it to the direct `require`
block, and `go.sum` is unchanged.

Strict coverage, asserted as `googleapi.Error` status and first error reason:

- The admin creates a bucket in `local-project`, reads and lists it, uploads with
  a single-request Writer (`ChunkSize=0`), reads metadata and bytes, lists
  objects, updates custom metadata, then deletes the object and the bucket.
- Anonymous object metadata, media reads and listing fail with 401 `required`;
  an unregistered bearer token fails the same calls with 401 `authError`.
- The object viewer reads metadata and bytes and lists objects, but uploads,
  metadata updates, deletes and bucket metadata reads fail with 403 `forbidden`.
  The rejected upload publishes nothing and the existing object stays unchanged.
- The object creator uploads a new object, but overwriting it fails with 403
  `forbidden` because an overwrite also needs `storage.objects.delete`; the
  original generation and bytes remain. Its metadata and media reads fail with 403.

`bucket_iam_test.go` exercises the pinned Storage SDK's `BucketHandle.IAM().Policy`
and `SetPolicy`. `TestStrictAuthBucketIAM` checks the default convenience bindings,
grants `roles/storage.objectViewer` to the principal with no project roles, reads
and lists that bucket's objects, and rejects access to another bucket and object
creation with 403. Both project and bucket viewers cannot get/set policies (403).
A stale-etag update fails with 412 `conditionNotMet` without removing the grant;
revocation denies metadata, media and listing while preserving the object.
`TestBucketIAMPermissive` verifies unauthenticated get/set, a fresh etag and stale
etag rejection. These tests use the existing clients and opt-in endpoints; no
new direct dependency or module version is needed.

Run only bucket IAM coverage with both isolated profiles:

```sh
GCS_MIMIC_GO_TEST_RUN='TestStrictAuthBucketIAM|TestBucketIAMPermissive' bash scripts/sdk-go.sh
```

`TestObjectList` additionally exercises prefix/delimiter/offset filters and one-entry
iterator/pager traversal. `TestObjectMetadataUpdate` verifies conditional Update,
fixed/custom metadata, empty-value and deletion behavior, and unchanged object bytes.
See the [listing](../../docs/en/slices/json-object-list.md),
[metadata](../../docs/en/slices/json-object-metadata.md) and
[multipart](../../docs/en/slices/json-object-multipart.md) contracts for limits.

Object IAM, `testIamPermissions`, conditional policies, ACLs, `allUsers` grants,
XML, gRPC, signing and resumable transfers are outside this harness's scope.
A clean skip or `-run XXX` is compilation evidence only;
successful object execution is recorded in the
[object media verification summary](../../docs/en/evidence/object-media.md) and
[raw SDK log](../../docs/en/evidence/object-media-go.txt).
No real-cloud differential tests are performed.

## Official sources

Storage v1.70.0 sources were checked on 2026-10-22; the remaining references
retain their original 2026-10-21 review date. The
[update evidence](../../docs/en/evidence/go-sdk-update.txt) records the release
review and full native suite results.

- [Go stable releases](https://go.dev/dl/?mode=json)
- [Go toolchain selection](https://go.dev/doc/toolchain)
- [Storage version metadata](https://proxy.golang.org/cloud.google.com/go/storage/@latest)
- [API client version metadata](https://proxy.golang.org/google.golang.org/api/@latest)
- [Storage v1.70.0 release notes](https://github.com/googleapis/google-cloud-go/blob/9a2913189e552da51b0fd60aa635027426f04d71/storage/CHANGES.md)
- [Pinned HTTP client source](https://github.com/googleapis/google-cloud-go/blob/9a2913189e552da51b0fd60aa635027426f04d71/storage/storage.go)
- [Pinned reader transport default](https://github.com/googleapis/google-cloud-go/blob/9a2913189e552da51b0fd60aa635027426f04d71/storage/reader.go)
- [Pinned Writer HTTP implementation](https://github.com/googleapis/google-cloud-go/blob/9a2913189e552da51b0fd60aa635027426f04d71/storage/http_client.go)
- [Pinned IAM implementation](https://github.com/googleapis/google-cloud-go/blob/9a2913189e552da51b0fd60aa635027426f04d71/storage/iam.go)
- [Pinned upload-type selection](https://github.com/googleapis/google-api-go-client/blob/0d9c67e59dc9291c6047cea77122907594b10994/internal/gensupport/media.go)
- [Object insert: media path, name, content type and create-if-absent](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert)
- [Object resource: metadata and checksum encoding](https://docs.cloud.google.com/storage/docs/json_api/v1/objects)
- [Object get: metadata, media, generation and conditions](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/get)
- [Object delete: generation and conditions](https://docs.cloud.google.com/storage/docs/json_api/v1/objects/delete)
- [Precondition failures](https://docs.cloud.google.com/storage/docs/request-preconditions)
- [Bucket delete requires an empty bucket](https://docs.cloud.google.com/storage/docs/json_api/v1/buckets/delete)
- [Live JSON Discovery contract](https://storage.googleapis.com/$discovery/rest?version=v1)
- [Module proxy and retraction contracts](https://go.dev/ref/mod)
- [setup-go v7.0.0 release](https://github.com/actions/setup-go/releases/tag/v7.0.0)
- [setup-go immutable commit](https://api.github.com/repos/actions/setup-go/git/ref/tags/v7.0.0)
