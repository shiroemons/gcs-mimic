# Distroless runtime verification

Verified on 2026-10-22 using the accompanying Dockerfile, smoke and dependency
checker changes. Runtime application code is unchanged.

## Scope and acceptance

The runtime moves from Debian slim to digest-pinned Distroless Debian 13 cc,
keeping the GNU/glibc build and numeric UID/GID 10001. Only the container binary
has its symbols stripped. No checksum, authorization, buffering, synchronization
or durability setting is weakened.

Acceptance requires both Linux architectures to build and pass the existing
health, networking, bucket, resource-limit and shutdown checks, plus binary
object media round trips across stop/start and down/up. Existing UID-10001 data
must remain usable. Compare complete images built from the same application
source and compiler inputs; report each architecture separately.

The daily dependency check must cover the new public GCR runtime index and
platform digests while retaining Docker Hub and Debian codename checks.

## Inputs

The comparison baseline is source commit `18a39ec` with its Debian slim runtime.
Both variants use Rust 1.99.1 and the committed Cargo.lock. The Distroless cc
nonroot index is
`sha256:b7e81cb1af38d9345680e68bcf564d388f96304e25222030fc8c051faf8565b3`.
The dependency snapshot in commit `bd55728` records both platform manifests and
all other inputs for this comparison. The current
[dependency snapshot](../../../compatibility/dependency-status.json) can advance
those pins through later tested updates.

The host is macOS arm64; linux/arm64 runs natively and linux/amd64 uses emulation.
Native amd64 and remote GitHub Actions execution remain unverified. This scope
does not establish byte-identical independent rebuilds, CPU/RSS improvements,
production hardening or complete third-party notice packaging.

## Size measurements

Complete application images were saved with `docker image save`, then their OCI
indexes were resolved to each Linux platform manifest. Compressed bytes are the
sum of the manifest's gzip layer descriptor sizes, excluding manifests, config,
attestations and transport overhead. This measures a fresh pull's layer payload;
no image was uploaded to a registry. Uncompressed bytes are decompressed layer
tar sizes, including tar metadata and padding, rather than host disk allocation.

| Platform | Debian compressed bytes | Distroless compressed bytes | Reduction | Debian uncompressed bytes | Distroless uncompressed bytes |
| --- | --- | --- | --- | --- | --- |
| linux/amd64 | 32,058,586 | 12,725,254 | 60.31% | 86,472,192 | 33,318,912 |
| linux/arm64 | 32,422,211 | 12,979,353 | 59.97% | 108,300,800 | 43,673,088 |

| Platform | Original binary bytes | Stripped binary bytes |
| --- | --- | --- |
| linux/amd64 | 5,195,784 | 4,165,656 |
| linux/arm64 | 5,143,080 | 3,952,288 |

Both final archives contain the two project license texts and a data directory
owned by UID/GID 10001. Neither contains `bin/sh` or `usr/bin/sh`. The measurement
uses OCI content, avoiding Docker Desktop's packed/unpacked cache accounting.
The two architectures are not compared with each other.

For a saved Buildx image, the compressed layer calculation can be reproduced with
Python's standard library, which is used only for this measurement:

```sh
docker image save --output "$TMPDIR/gcs-image.tar" gcs-mimic:dev
python3 - "$TMPDIR/gcs-image.tar" <<'PY'
import json
import sys
import tarfile

with tarfile.open(sys.argv[1]) as archive:
    def read_blob(digest):
        return json.load(archive.extractfile('blobs/' + digest.replace(':', '/')))

    for image in json.load(archive.extractfile('index.json'))['manifests']:
        for entry in read_blob(image['digest'])['manifests']:
            platform = entry.get('platform', {})
            if platform.get('os') != 'linux':
                continue
            layers = read_blob(entry['digest'])['layers']
            for layer in layers:
                name = 'blobs/' + layer['digest'].replace(':', '/')
                assert archive.getmember(name).size == layer['size']
            print(platform, sum(layer['size'] for layer in layers))
PY
```

## Commands and results

| Command or check | Result |
| --- | --- |
| `just pre-push` | PASS, exit 0; final Rust/shell, Go SDK and both container architecture checks |
| Rust workspace tests through the shared check | PASS; 142 tests, 0 failed, 0 ignored |
| `cargo test --package xtask --lib dependencies::tests --locked --offline` | PASS; 10 tests, including Docker Hub default auth, unauthenticated GCR, unknown-registry rejection and OCI headers |
| Go vet and uncached SDK tests through the SDK harness | PASS; full suite |
| `bash scripts/smoke-containers.sh` through the Justfile | PASS; native arm64 and emulated amd64, including binary media across stop/start and down/up |
| `just check-dependencies` | PASS, exit 0; every Rust, crate, Actions, Docker Hub, GCR, Debian and Go input CURRENT |
| `shellcheck scripts/check.sh scripts/test-check.sh scripts/smoke-containers.sh scripts/sdk-go.sh` | PASS, exit 0 |
| `actionlint .github/workflows/ci.yaml .github/workflows/dependency-check.yaml` | PASS, exit 0 |
| `just --fmt --check` | PASS, exit 0 |
| `cargo xtask compatibility render` and `cargo xtask compatibility check` after the ledger update | PASS, exit 0; 588 feature rows and both generated views agree |
| Markdownlint on changed Markdown | PASS; 0 issues, with MD013 disabled for existing long tables, URLs and commands |

A separate native-arm64 named-volume migration check created a bucket and a
12-byte binary object with the baseline Debian image, stopped it with exit zero,
and started Distroless with the same volume. Exact download bytes matched before
and after an overwrite by Distroless, and the new process also stopped with exit
zero. Both images ran as UID/GID 10001 with a read-only root and the same resource
limits. The disposable volume was removed after the check.

The smoke fixture includes NUL and 0xff bytes. It verifies upload identity/size,
exact downloaded bytes, persistence across stopping and recreating containers,
object deletion and subsequent 404, bucket CRUD, host/peer DNS, resource settings,
SIGINT/SIGTERM and explicit reset. No real Google service or user data is used.

The harnesses removed their isolated containers, networks and volumes. The four
temporary baseline/test image tags and both measurement archives were removed
after verification; shared Docker caches and unrelated resources were retained.

## Remaining limits

Image signatures and vulnerability scans were not run. Digest pinning and removal
of the shell do not establish absence of vulnerabilities. CPU, RSS, cgroup memory,
latency, power-loss durability and native amd64 execution are not measured here.
musl/static migration and runtime performance optimization remain separate work.
