# Official Go JSON SDK tests

This module pins Go 1.27.3, `cloud.google.com/go/storage` v1.69.0 and
`google.golang.org/api` v0.306.0, verified on 2026-10-21. `go.sum` records module
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

`TestBucketLifecycle` and `TestObjectMediaLifecycle` skip with an explicit message
when `GCS_MIMIC_ENDPOINT` is unset. `-run XXX` compiles every package without
running tests. To build and start an isolated server, run from the repository root:

```sh
bash scripts/sdk-go.sh
```

To select tests, pass a Go `-run` regular expression with
`GCS_MIMIC_GO_TEST_RUN`. Vet still checks all packages. For example, the bucket
suite can run independently:

```sh
GCS_MIMIC_GO_TEST_RUN='TestEmulatorEndpoint|TestBucketLifecycle' bash scripts/sdk-go.sh
```

The runner requires Bash, Rust, Go and jq. It builds with the pinned Rust toolchain,
selects an available loopback port, checks the bound port in the ready JSON log, probes
`/_internal/ready`, then runs vet and uncached verbose tests. The server must
support `serve --listen IP:PORT --data-dir PATH`. A trap stops the server, reports
its failure log and deletes only this run's temporary directory.

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
Storage v1.69.0 accepts a full origin or a host:port value and adds `/storage/v1/`.
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
is a media upload. The supported upload path therefore uses the same guarded HTTP
client directly. A Writer with `ChunkSize=0` explicitly exercises multipart and
must return 501 `notImplemented` with a nonempty message, without creating an
object. This status is the emulator's documented unsupported-semantics deviation,
not the behavior of Google's multipart API. This harness does not establish SDK
Writer upload compatibility.

ACL/IAM, updates, ranges, XML, gRPC, signing and resumable transfers are outside
this harness's scope. A clean skip or `-run XXX` is compilation evidence only;
successful object execution is recorded in the
[object media verification summary](../../docs/en/evidence/object-media.md) and
[raw SDK log](../../docs/en/evidence/object-media-go.txt).
No real-cloud differential tests are performed.

## Official sources

All checked on 2026-10-21:

- [Go stable releases](https://go.dev/dl/?mode=json)
- [Go toolchain selection](https://go.dev/doc/toolchain)
- [Storage version metadata](https://proxy.golang.org/cloud.google.com/go/storage/@latest)
- [API client version metadata](https://proxy.golang.org/google.golang.org/api/@latest)
- [Storage v1.69.0 emulator documentation](https://pkg.go.dev/cloud.google.com/go/storage@v1.69.0)
- [Pinned HTTP client source](https://github.com/googleapis/google-cloud-go/blob/9981d08a82c88e29c2250831a4449f1966418609/storage/storage.go)
- [Pinned reader transport default](https://github.com/googleapis/google-cloud-go/blob/9981d08a82c88e29c2250831a4449f1966418609/storage/reader.go)
- [Pinned Writer HTTP implementation](https://github.com/googleapis/google-cloud-go/blob/9981d08a82c88e29c2250831a4449f1966418609/storage/http_client.go)
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
