# Docker Hub mirror verification

Scope: `DIST-004`, on 2026-10-28. Publication remains PARTIAL. No Docker Hub
writes, release tags or GitHub Releases were performed while preparing this
evidence.

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

Against the published `ghcr.io/shiroemons/gcs-mimic:0.2.1`, read anonymously:

- `docker buildx imagetools inspect --format '{{.Manifest.Digest}}'` returned
  `sha256:d3846b7b42f5dc60a5c50f0bfb5248cb0fe35ebec0361eaf1eaf271ee34c73bb`,
  equal to the `index_digest` in the v0.2.1 `CONTAINER-DIGESTS.json` asset.
- `{{json .SBOM}}` and `{{json .Provenance}}` on the multi-platform index return
  objects keyed by `linux/amd64` and `linux/arm64`, with SPDX-2.3 documents and
  the BuildKit SLSA v1 build type. The mirror script relies on this shape.

The Docker Hub API reported `shiroemons/gcs-mimic` as public with zero tags.

`actionlint` on all workflows and `shellcheck` on the new scripts: PASS.

## Limits

No Docker Hub push, cross-registry copy, alias assignment or anonymous Docker Hub
pull has run. The environment token scope and branch policy were observed through
the GitHub API, not exercised by a workflow.
