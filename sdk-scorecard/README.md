# SDK scorecard

<!-- markdownlint-configure-file {"MD013": false} -->

The scorecard measures how much of the official Go and Ruby Cloud Storage SDK
surface works unmodified against a local emulator. It runs the same probes,
inputs and pass criteria against gcs-mimic, fake-gcs-server and
gcp-storage-emulator, so results are comparable across products and over time.

It is a compatibility comparison, not a performance benchmark, and not an
implementation-status source: `compatibility/features.json` remains the only
status ledger. A passing probe means an ordinary SDK user observes the expected
result for that workflow; it does not mean the underlying API method is
`IMPLEMENTED`.

## Scoring

A product that passes every Go and Ruby case scores 100.

- Each SDK contributes 50 points.
- Within an SDK, every case has equal weight: `50 × passed / total`.
- Score = Go half + Ruby half, rounded to one decimal.

Ruby has no gRPC transport, so the Go probe has more cases than the Ruby probe.
Equal weighting is a deliberate simplification: an ACL round trip counts as much
as resumable upload. Read the per-area table in each `summary.md` alongside the
total.

## Pass criteria

Each case verifies the observable effect, not just the absence of an error:

- settings are read back after being written (labels, lifecycle, CORS, UBLA,
  PAP, soft-delete policy, autoclass, ACL entries, IAM bindings);
- enforcement is exercised (retention and holds reject deletion with 403,
  failed preconditions return 412 and leave the object unchanged, a wrong
  CRC32C leaves no object, a customer-supplied-key object is unreadable
  without its key);
- bytes are compared after every upload, copy, rewrite, compose, move, range,
  signed and POST policy transfer;
- listing cases check exact names, prefixes and page sizes.

An emulator that accepts a setting but ignores it fails the case.
Emulator-specific features that the SDKs do not expose (strict local
authentication, seeding, sync directories, admin routes) are outside the
scorecard.

## Areas

| Area | Go | Ruby | Scope |
| --- | --- | --- | --- |
| bucket | 20 | 20 | Bucket CRUD/listing, labels, preconditions, versioning, lifecycle, CORS, retention and lock, UBLA, PAP, IAM, soft-delete policy, autoclass, ACL/default ACL, notifications, HMAC keys, service account |
| object | 27 | 27 | Uploads (single, resumable, metadata, checksums), range reads, PATCH, delete, generation/metageneration conditions, listing (delimiter, paging, offsets, glob), copy, rewrite, compose, move, ACL, holds, object retention, custom time, storage class, gzip transcoding, CSEK, soft delete/restore |
| signed | 4 | 4 | V4 GET/PUT, V2 GET, V4 POST policy |
| grpc | 6 | – | `storage.NewGRPCClient`: bucket CRUD, write/read, list, update/delete, MultiRangeDownloader, appendable objects |

Case IDs are shared between the SDKs. The probes live in
[`go/main.go`](go/main.go) and [`ruby/probe.rb`](ruby/probe.rb).

## Pinned inputs

| Input | Version |
| --- | --- |
| Go SDK | `cloud.google.com/go/storage` v1.70.0 (`go/go.mod`, same graph as `sdk-tests/go`) |
| Ruby SDK | `google-cloud-storage` 1.62.1 (`ruby/Gemfile.lock`, same graph as `sdk-tests/ruby`) |
| gcs-mimic | Release build of the checked-out source; the commit is recorded in `*.meta.json` |
| fake-gcs-server | `fsouza/fake-gcs-server:1.56.1` |
| gcp-storage-emulator | `oittaa/gcp-storage-emulator:v2026.07.19` |

Update the pins in `run.sh` and the module files deliberately; record the
change in the next results directory rather than rewriting old results.

## Emulator configuration

Every product runs over plain HTTP on a loopback port, with a fresh in-memory or
temporary data store, in its most capable documented configuration:

- gcs-mimic: `serve --scheme http --grpc-listen ...` with the default
  permissive authentication profile.
- fake-gcs-server: `-scheme http -backend memory` (the memory backend is the
  only one that retains object versions); gRPC cases use the shared HTTP port.
- gcp-storage-emulator: `start --in-memory`; it has no gRPC listener, so the
  gRPC cases fail by timeout.

Signed URLs use the public test key in `fixtures/signing`. Clients are
anonymous and never contact Google Cloud or the metadata server.

## Running

Requires Bash, Rust, Go, Ruby 4.0.9 with Bundler, Docker, curl and jq.

```sh
bash sdk-scorecard/run.sh                       # all products, results/<UTC date>/
bash sdk-scorecard/run.sh gcs-mimic             # one product
bash sdk-scorecard/run.sh --out "$TMPDIR/sc"    # scratch run
bash sdk-scorecard/score.sh sdk-scorecard/results/2026-10-29   # rescore recorded results
```

`SCORECARD_RUN=<substring>` restricts both probes to matching case IDs when
debugging a single case against a running emulator:

```sh
cd sdk-scorecard/go
SCORECARD_ENDPOINT=http://127.0.0.1:4443 SCORECARD_RUN=object.compose go run .
```

## Results

Each `results/<date>/` directory keeps the raw evidence:

- `<emulator>.meta.json`: product, version and measurement time;
- `<emulator>.go.jsonl`, `<emulator>.ruby.jsonl`: one line per case with the
  failure detail;
- `summary.md`: totals, per-area counts and the case matrix.

Compare runs only when their case lists match; adding or removing a case changes
every product's denominator. When the probes change, rerun all products and
start a new results directory.

## Limitations

- Equal case weights do not reflect how often each feature is used.
- Only permissive, HTTP and default-region behavior is measured. Strict
  authorization, TLS, browser CORS and crash recovery have their own evidence
  under `docs/en/evidence`.
- A case is pass/fail; partial support inside a case is not credited.
- Error-message text and exact HTTP headers are not compared.
