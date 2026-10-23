# Container distribution through GHCR

<!-- markdownlint-configure-file {"MD013": {"tables": false}} -->

Publication infrastructure is configured for
`ghcr.io/shiroemons/gcs-mimic` on Linux amd64 and arm64. Public image publication
is pending. The [Docker usage guide](container-usage.md) supplies the consumer
workflow; [local preparation evidence](evidence/container-onboarding.md) records
both image builds, container smoke checks and the README walkthrough.
The first release retains the [roadmap requirements](roadmap.md#first-useful-release).

## Tags and immutable identity

| Reference | Meaning | Update rule |
| --- | --- | --- |
| `X.Y.Z` | Exact stable product version | Never replace with different contents |
| `X.Y` | Highest published patch in that minor series | Advance only |
| `latest` | Highest published stable product version | Advance only |
| `@sha256:...` | Exact multi-platform index or platform manifest | Content addressed |

For example, Git tag `v0.2.3`, Cargo version `0.2.3`, release archives and image
tag `0.2.3` describe the same version and source commit. `0.2` can advance to
`0.2.4`, while `latest` can advance to a new minor with breaking changes during
0.x development. Neither is a compatibility promise beyond the release policy.
There is no broad `0` tag, development `edge` tag, or main-push publication.

Use an exact version or, preferably, a digest in reproducible CI. Human-readable
version immutability is enforced by this workflow and repository access policy;
GHCR itself permits a sufficiently privileged writer to move tags. Limit package
write access and never change an already published exact version manually.

## Release flow

1. Prepare the version, lockfile, feature ledger and
   `docs/en/releases/X.Y.Z.md`. Run freshness and pre-push checks and follow the
   [native release procedure](binary-distribution.md#verification-and-release-flow).
2. Manual **Release binaries** execution verifies the existing Foundation checks
   and eight native binary targets without writing to GHCR. The GHCR publication
   workflow runs only for the future `vX.Y.Z` tag path.
3. A matching version tag repeats those gates and creates the binary GitHub
   Release. That successful job directly calls the reusable GHCR workflow. It
   does not rely on a `release: published` event: a Release created with
   `GITHUB_TOKEN` does not trigger a second workflow through that event.
4. Each Linux CPU builds on its native runner, checks `--version`, then runs the
   complete container smoke suite on the built image: networking, media CRUD,
   persistence, non-root/read-only limits and shutdown. Credentials are exposed
   to the registry-login step only after these checks.
5. The same builder cache and fixed build arguments export SBOM/provenance and
   push each platform by digest without assigning a release tag. The published
   config digest must equal the config digest of the tested image. Both platform
   jobs must succeed before any stable image tags are assigned.
6. The manifest job verifies the unchanged remote Git tag, package visibility,
   both platform indexes and their attestations, then combines the two indexes.
   Existing exact versions may be reused only when their manifest descriptors
   match the verified inputs; different contents are rejected. The job serializes
   promotion and compares stable GitHub Release versions numerically before
   advancing minor/latest aliases, so older releases cannot roll them back.
7. The final index digest and per-platform digests are attached to the GitHub
   Release as `CONTAINER-DIGESTS.json`. Anonymous registry access must succeed.
   Consumers can choose the index digest for automatic CPU selection or a
   platform digest for an explicitly selected CPU.

GitHub Release creation, registry pushes, alias updates and asset attachment are
separate remote operations. They are not an atomic transaction. A binary Release
can exist while GHCR publication is incomplete, and a failed matrix can leave
untagged platform indexes in GHCR. Inspect the workflow and digest report before
claiming a complete container release.

After a transient failure, rerun **failed jobs**, retaining the successful jobs'
digest artifacts. Exact tags and existing digest reports are accepted only if
their contents match, and are never overwritten. A rerun that rebuilds platform
attestations can produce different digests; do not force it over an existing
version. If inputs change, prepare a new product version. Several simultaneous
releases can also hit GitHub's single-pending-job concurrency limit; inspect and
rerun a cancelled publication rather than assuming it completed.

## First publication and access

The GitHub repository is public. GHCR packages nevertheless start **private** on
first publication. The first authorized run can upload its untagged platform
indexes and then stop before assigning stable tags. The owner must open the
package's **Package settings**, change visibility to **Public**, confirm the
repository association and Actions access, then rerun the failed manifest job.
This is a one-time registry setup step, not a step performed by the current slice.
Do not publish a premature release just to initialize the package.

The workflow uses its short-lived `GITHUB_TOKEN`, with `packages: write` confined
to publication jobs; it does not require a personal access token. The manifest
job additionally needs `contents: write` to attach the digest report. Checkout
does not persist credentials. Docker authentication uses a temporary config and
is logged out at job completion. PRs and ordinary main pushes cannot enter the
publication path. If an unrelated existing GHCR package occupies the name, link
and authorize it deliberately; an authorization failure is not proof of absence.

After publication is verified, a consumer can run an exact version:

```sh
docker pull ghcr.io/shiroemons/gcs-mimic:X.Y.Z
docker run --rm ghcr.io/shiroemons/gcs-mimic:X.Y.Z --version
```

Replace `X.Y.Z` with a version that actually exists. For digest pinning, use
`ghcr.io/shiroemons/gcs-mimic@sha256:<index-digest>` from the release report. Bind
the server only to local development networks and follow the existing
[configuration and volume guide](getting-started.md).

## Notices, SBOM and provenance

Images include both project license texts and Cargo/Rust/SQLite/crc32c/GCC
notices under `/usr/share/licenses/gcs-mimic`. Distroless's OS copyright notices,
common license texts and package records remain in their original locations.
The added index points to those records; build-image OS packages are not treated
as though they all ship in the runtime image.

The BuildKit image and official SBOM generator are pinned by multi-platform
digest and checked daily with the other container inputs. The OCI indexes carry
an SPDX SBOM and SLSA provenance. These improve inspection and traceability;
they are not signatures, a vulnerability scan, proof of complete dependency
detection, or evidence of byte-identical builds. The locked Cargo license index
is included separately because automatic SBOM detection may omit statically
linked Rust crates. Hosted runner Docker/Buildx versions are logged but are not
byte-pinned runtime inputs. No signing key or OIDC signing is added.

## References

- [GitHub Container registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)
- [Workflow event recursion rules](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow)
- [Docker multi-platform builds](https://docs.docker.com/build/ci/github-actions/multi-platform/)
- [Build attestations](https://docs.docker.com/build/metadata/attestations/)
