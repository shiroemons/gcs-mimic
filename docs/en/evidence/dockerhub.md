# Docker Hub mirror verification

<!-- markdownlint-configure-file {"MD013": {"code_blocks": false}} -->

Scope: `DIST-004`, on 2026-10-28 and 2026-10-29. Status remains PARTIAL: the
authorized manual backfills of 0.2.1 and 0.3.0 succeeded, while the first
tag-triggered mirror, for 0.3.0, failed before copying anything.

## Authorized 0.2.1 backfill

The owner authorized adding `main` to the `dockerhub` environment's deployment
refs and a manual run from `main` at `60ab255`.
[Run 40693113466](https://github.com/shiroemons/gcs-mimic/actions/runs/40693113466)
(**Mirror release container to Docker Hub**, version `0.2.1`) passed every step.
Its log ends with:

```text
PASS: docker.io/shiroemons/gcs-mimic:0.2.1 at sha256:d3846b7b42f5dc60a5c50f0bfb5248cb0fe35ebec0361eaf1eaf271ee34c73bb; SPDX SBOM, SLSA provenance and anonymous access verified
```

Independent checks afterwards:

- `docker buildx imagetools inspect --format '{{.Manifest.Digest}}'` reports
  `sha256:d3846b7b42f5dc60a5c50f0bfb5248cb0fe35ebec0361eaf1eaf271ee34c73bb` for
  Docker Hub tags `0.2.1`, `0.2` and `latest`, equal to the GHCR index and the
  v0.2.1 `CONTAINER-DIGESTS.json` report.
- `{{json .SBOM}}` on the Docker Hub tag returns SPDX-2.3 for `linux/amd64` and
  `linux/arm64`.
- `docker run --rm --pull always --platform linux/amd64|linux/arm64
  docker.io/shiroemons/gcs-mimic:0.2.1 --version` prints `gcs-mimic 0.2.1` for
  both platforms. The local host is macOS ARM64; amd64 runs under emulation.

## 0.3.0 tag release and backfill

On 2026-10-29 the `v0.3.0` tag started
[run 40794280766](https://github.com/shiroemons/gcs-mimic/actions/runs/40794280766)
(**Release binaries**). Validation, all native builds, the GitHub Release and
GHCR publication passed, so the tag-triggered path reached
`publish-dockerhub / mirror` for the first time. That job failed at
`docker login`: `REGISTRY_USER` resolved from the environment variable, but
`REGISTRY_TOKEN` was empty and the log reports
`Cannot perform an interactive login from a non TTY device`. The caller passed
no secrets to the reusable workflow, so the environment secret did not reach the
called job. `binaries.yaml` now passes `secrets: inherit`; the next tag release
must confirm the fix.

The owner had authorized publishing 0.3.0 to every registry, so a manual run
from `main` at `7054c5e` completed it:
[run 40796866383](https://github.com/shiroemons/gcs-mimic/actions/runs/40796866383)
passed and its log ends with:

```text
PASS: docker.io/shiroemons/gcs-mimic:0.3.0 at sha256:c6de2cef4ea4aae7a246047f8a1973055faefc13f5a1d109016b026f6590e548; SPDX SBOM, SLSA provenance and anonymous access verified
```

Independent checks afterwards:

- `docker buildx imagetools inspect` reports
  `sha256:c6de2cef4ea4aae7a246047f8a1973055faefc13f5a1d109016b026f6590e548` for
  Docker Hub tags `0.3.0`, `0.3` and `latest`, equal to the GHCR tags.
- `{{json .SBOM}}` on the Docker Hub tag returns SPDX-2.3 for `linux/amd64` and
  `linux/arm64`, and the index lists one attestation manifest per platform.
- `docker run --rm --pull always --platform linux/amd64|linux/arm64
  docker.io/shiroemons/gcs-mimic:0.3.0 --version` prints `gcs-mimic 0.3.0` for
  both platforms.

## Executed offline checks

`bash scripts/test-container-mirror.sh`: PASS without registry, Docker Hub API or
GitHub writes. Mocked external commands cover:

- a non-stable version argument;
- a private or unreachable Docker Hub repository, and Docker Hub tag-listing
  failures;
- a digest report for another image, a moved source Git tag, and a GHCR version
  tag that differs from the digest report;
- missing per-platform SBOMs and foreign provenance on the GHCR source;
- a pagination link outside the repository's tag endpoint;
- an existing Docker Hub version tag with different contents;
- a first copy that keeps the GHCR index digest, then assigns `X.Y` and `latest`;
- an identical-input retry across two tag pages that skips the exact version;
- a newer stable Release that withholds `latest`;
- a policy result outside the version, minor and `latest` tags;
- an anonymous-access failure after the copy.

No case prints the mocked GitHub token. The mocks exercise local control flow;
they do not establish the behavior of the real services.

## Executed read-only registry observations

Before the backfill, against the published
`ghcr.io/shiroemons/gcs-mimic:0.2.1`, read anonymously:

- `docker buildx imagetools inspect --format '{{.Manifest.Digest}}'` returned
  `sha256:d3846b7b42f5dc60a5c50f0bfb5248cb0fe35ebec0361eaf1eaf271ee34c73bb`,
  equal to the `index_digest` in the v0.2.1 `CONTAINER-DIGESTS.json` asset.
- `{{json .SBOM}}` and `{{json .Provenance}}` on the multi-platform index return
  objects keyed by `linux/amd64` and `linux/arm64`, with SPDX-2.3 documents and
  the BuildKit SLSA v1 build type. The mirror script relies on this shape.

The Docker Hub API reported `shiroemons/gcs-mimic` as public with zero tags.

`actionlint` on all workflows and `shellcheck` on the new scripts: PASS.

## Limits

The tag-triggered path from **Release binaries** through GHCR publication to the
Docker Hub job has not run; the next version tag exercises it. An identical-input
rerun against Docker Hub has not run. Docker Hub permits a token holder to move
tags, so exact-version immutability depends on the workflow and token custody.
