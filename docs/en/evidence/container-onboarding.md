# Docker onboarding verification

<!-- markdownlint-configure-file
{"MD013":{"tables":false,"code_blocks":false}}
-->

Checked on **2026-10-24** using Docker Desktop on Apple Silicon. The images used
build inputs matching commit `998296daa40c47af498bd7abf05acfe1b906a4d2`.
Later edits change documentation, planning metadata and the Japanese matrix
output path in xtask.
No server code, dependency, Dockerfile or release workflow changed.

## Local images and checks

Both images were built from the current checkout with the existing pinned
Dockerfile and matching BuildKit cache. ARM64 ran natively; AMD64 ran under
Docker Desktop emulation. These are local verification images, not published
release artifacts.

```sh
cargo +1.99.1 xtask dependencies check
docker buildx build --platform linux/arm64 --load \
  --tag gcs-mimic:preview-20261024-arm64 .
docker buildx build --platform linux/amd64 --load \
  --tag gcs-mimic:preview-20261024-amd64 .
bash scripts/smoke-containers.sh \
  --image gcs-mimic:preview-20261024-arm64 linux/arm64
bash scripts/smoke-containers.sh \
  --image gcs-mimic:preview-20261024-amd64 linux/amd64
bash scripts/test-container-release.sh
```

All commands exited **0**. Freshness reported every checked input `CURRENT`.
Each smoke run verified isolated readiness, host/peer access, bucket/object media
CRUD, bucket CORS, stop/start and recreate persistence, intentional reset of its
own disposable data, non-root/read-only resource settings, shutdown and seed
initialization without replay over later mutations or deletions.

The release-script self-check used mocks without network publication. It passed
input/API-error handling, immutable retries, attestation validation and alias
selection. It does not verify a real registry push.

| Platform | Built image configuration digest |
| --- | --- |
| `linux/arm64` | `sha256:9a434018fd049567446ea9dd5dcff68227246f186d84737ea662c7cd9f99c818` |
| `linux/amd64` | `sha256:1d10e37c9b63c64d3f89d647deb51088467a4865fcc58b7011017d682cdcf6ef` |

These are image **configuration** digests from the build output, not registry
index digests or consumer pull references. The native image also has the local
convenience tag `gcs-mimic:preview`.

## Consumer walkthrough

An ad hoc Node assertion probe extracted the README's Compose and shell blocks
and the Docker usage guide's `docker run` and CORS examples. It substituted the
verified local image, free loopback host ports and corresponding advertised
origins, and used unique container/project/volume names. Application containers
did not mount the checkout or require a build context. The temporary probe is
not a committed regression test.

```text
PASS: README Compose block starts without source and bucket/upload/read prints hello
PASS: separate bucket CORS, API changes, versioning setting and object bytes survive recreate
PASS: documented docker run uses HTTP and its explicit readiness probe succeeds
```

The CORS checks used two buckets with different allowed origins, verified each
allowed and denied preflight, and retained API-modified rules after recreation.
The versioning assertion checked the persisted bucket setting; this probe did
not add new version-history behavior evidence.

The final `docker run` example disables the image's default HTTPS probe and uses
`docker exec` for an HTTP readiness check. Docker's string `--health-cmd` cannot
run in the shell-free image; the Compose example uses an exec-form probe.

## Remaining publication work

No image was pushed and no GitHub Release was created. A read-only release list
returned no releases. The first publication still needs the chosen version's
release notes, the required native hosted CI/platform gates and authorized tag
publication. The current workflow publishes containers after the binary Release,
then checks GHCR public visibility, attestations, immutable tags and anonymous
access. Local verification does not replace those remote gates.

SBOM/provenance publication, registry pulls, all native archive targets, the full
SDK/browser suites and real-cloud behavior were not re-run in this preparation.
Bucket initialization files and bidirectional filesystem synchronization remain
requirements only. No feature status was promoted.
