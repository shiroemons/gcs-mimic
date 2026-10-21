# Official Go JSON SDK bucket tests

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
GOTOOLCHAIN=auto go test -v ./...
```

`TestBucketLifecycle` skips with an explicit message when `GCS_MIMIC_ENDPOINT` is
unset. To build and start an isolated server, run from the repository root:

```sh
bash scripts/sdk-go.sh
```

The runner requires Bash, Rust, Go and jq. It builds with the pinned Rust toolchain,
binds `127.0.0.1:0`, reads the selected port from the ready JSON log, probes
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
allow a real Cloud Storage endpoint. The suite creates five randomly named
buckets under a unique test project and deletes them, including cleanup on test
failure; use an isolated emulator.

The test sets the documented `STORAGE_EMULATOR_HOST` to the validated origin.
Storage v1.69.0 accepts a full origin or a host:port value and adds `/storage/v1/`.
`storage.NewClient` selects HTTP, `storage.WithJSONReads` selects JSON reads, and
`option.WithoutAuthentication` prevents credential discovery. Client metrics are
disabled. The HTTP transport disables proxies, rejects other request origins,
pins `localhost` dialing to loopback, and refuses redirects, preventing fallback
to Google or the metadata server. Each request and the whole test have deadlines.

Coverage: create with project, get identity/metageneration/creation time,
project-scoped listing with a small page size, prefix filtering across four
explicit one-item pages, duplicate create as `googleapi.Error` 409, delete,
get-after-delete as `storage.ErrBucketNotExist`, and missing delete as
`googleapi.Error` 404. SDK version and transport appear in verbose test output.
Objects, ACL/IAM, updates, XML and gRPC are outside this harness's scope. A clean
skip is compilation evidence only; successful emulator execution must be recorded
separately in the compatibility ledger.

## Official sources

All checked on 2026-10-21:

- [Go stable releases](https://go.dev/dl/?mode=json)
- [Go toolchain selection](https://go.dev/doc/toolchain)
- [Storage version metadata](https://proxy.golang.org/cloud.google.com/go/storage/@latest)
- [API client version metadata](https://proxy.golang.org/google.golang.org/api/@latest)
- [Storage v1.69.0 emulator documentation](https://pkg.go.dev/cloud.google.com/go/storage@v1.69.0)
- [Pinned HTTP client source](https://github.com/googleapis/google-cloud-go/blob/9981d08a82c88e29c2250831a4449f1966418609/storage/storage.go)
- [Live JSON Discovery contract](https://storage.googleapis.com/$discovery/rest?version=v1)
- [Module proxy and retraction contracts](https://go.dev/ref/mod)
- [setup-go v7.0.0 release](https://github.com/actions/setup-go/releases/tag/v7.0.0)
- [setup-go immutable commit](https://api.github.com/repos/actions/setup-go/git/ref/tags/v7.0.0)
