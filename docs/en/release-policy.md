# Versioning and licensing policy

[日本語](../ja/release-policy.md)

## Product version and compatibility

gcs-mimic follows [Semantic Versioning 2.0.0](https://semver.org/), with the additional compatibility rules below during initial development. The root `Cargo.toml` package version is the product version and supplies `--version`. The latest release is `0.13.0`; a manifest version alone does not establish that a release exists. The internal xtask package is not a separately released product.

The product version is independent of Google API versions, SDK versions and the
catalog schema version. The [feature ledger](../../compatibility/features.json)
defines the supported API subset; a product version does not claim complete
Google Cloud Storage compatibility.

During `0.x` development, use these rules:

| Change | Version update | Example |
| --- | --- | --- |
| Compatible bug or security fix | Patch | `0.1.0` to `0.1.1` |
| Feature or deprecation | Minor; reset patch to zero | `0.1.1` to `0.2.0` |
| Breaking change | Minor; reset patch to zero | `0.2.1` to `0.3.0` |

This is a project promise stricter than SemVer's general allowance for unstable
`0.x` APIs. A patch release must preserve the documented contract. Judge dependency
and container-base updates by their observable effects using the same rules.
A correction to match Google behavior is still a breaking change if it requires
users to change a documented integration or migrate persistent data.

Compatibility covers documented CLI commands and options, environment variables,
configuration formats and defaults, supported API behavior, supported runtime
platforms, and access to persistent data. Internal functions, implementation
details and human-readable diagnostic wording are not stable interfaces.

Document breaking changes and migration steps in release notes. Catalog schema
numbers are separate from application versions; schema changes that preserve the
documented upgrade path need not be breaking. Never silently reset or discard an
existing data directory to make an upgrade work. Document required migrations,
backup steps and downgrade restrictions; downgrade compatibility is not assumed.

## Progress through 0.x

Release useful increments while compatibility work continues. Do not keep adding
released functionality under `0.1.0` until the whole roadmap is complete.
For example, after the first `0.1.0` release, a bucket initializer can ship as
`0.2.0`, a compatible correction as `0.2.1`, and directory synchronization as
`0.3.0`. These are examples, not reserved feature versions or dates. Seed
extensions and data import can each ship in another minor release; neither must
wait for synchronization. Parallel work is assigned versions at integration.

Choose the next version from the last published release and its actual changes.
Use the highest required increment when combining changes. Documentation-only
edits do not require a product release. Update the application manifest and
lockfile, user examples, release notes and artifact references together when
preparing the chosen release. A local preview image is not a published version.

## The 1.0.0 gate

`1.0.0` is a stability promise, not a completeness claim.
It commits to the public contract described above, so users can upgrade within `1.x` without changing a documented integration or losing data.
Every supported capability keeps its recorded scope; rows that are `PARTIAL` in the [feature ledger](../../compatibility/features.json) stay `PARTIAL` and are documented as such.
All of these must pass:

1. **Freeze the public contract.**
   Document the stable surface: CLI commands and options, environment variables, configuration file formats and defaults, data directory access, and the supported API subset with its recorded limits.
   Before the first release candidate, review that surface for names, aliases and defaults that should not be kept, and ship every remaining breaking change in a `0.x` minor release.
2. **Upgrade data directories from every supported release.**
   A data directory written by each supported release opens with the new build and serves the same buckets, objects, generations, metadata and bytes.
   The fixtures come from the published images through `scripts/released-data-fixtures.sh` and are checked by `tests/it_import_seed/released_data_upgrade.rs`; each release adds its own fixture.
   Supported releases are `0.8.0` and later; see [withdrawn releases](#withdrawn-releases).
3. **Soak a release candidate.**
   Publish `1.0.0-rc.N` and keep it for at least 14 days without a breaking change.
   A breaking change starts a new candidate.
4. **Verify the final source once.**
   Run the native, cross-target, SDK, container and browser gates on the release commit.
   Results from earlier checkpoints do not substitute for this run.
5. **State the migration scope.**
   The [fake-gcs-server migration guide](fake-gcs-server-migration.md) lists the supported migration workflows and the remaining gaps against the pinned reference without claiming full replacement.

A complete functional superset of the pinned fake-gcs-server baseline remains a goal, tracked by the [full-superset gate](fake-gcs-server-migration.md#delivery-and-full-superset-gate).
It is not a `1.0.0` condition.
The phrase "fully backward-compatible superset" is used only after that gate passes, in whichever release that happens.
Full Google Cloud Storage API coverage is likewise a separate goal.

After `1.0.0`, breaking public-contract changes increment major, compatible additions increment minor, and compatible fixes increment patch.

## Withdrawn releases

Releases `0.1.0` through `0.7.0` were withdrawn on 2026-11-02.
They predate a usable public contract, and keeping them available invited use that the project does not support.
Their GitHub Releases, native archives, and exact version and minor alias container tags on GHCR and Docker Hub were deleted.
Their Git tags and their [release notes](releases/) remain as source history.

Data directories written by these releases are not supported.
A current build may still open one, but that upgrade is untested; start with an empty data directory or reseed it.
This withdrawal is a one-time exception to the immutability rule below, made before `1.0.0`.

## Release identity and publication

Choose the product version when preparing a release, not for every commit or
push. Git release tags use `vX.Y.Z`; binary metadata and the exact container version
tag use `X.Y.Z`. They must refer to the same product version and source commit.

Published release tags and artifacts are immutable: do not move a release tag,
replace a binary, or overwrite an exact container version tag. Publish corrections
or security rebuilds under a new version, even if only dependencies or the base
image changed. Record artifact checksums and image digests with each release.

The first general developer release retains the
[roadmap's P1 plus essential P2 public/private and URL target](roadmap.md#first-useful-release).
Before publication, update the version and lockfile together, reconcile the
feature ledger and documentation, run dependency freshness and pre-push checks,
and verify the release's supported platforms in CI. Follow the existing
[dependency release gates](dependency-policy.md#ci-lanes). Release notes describe
the final changes, compatibility impact, migration instructions, tested platforms
and known limitations.

The [native distribution procedure](binary-distribution.md) defines the
archive targets, manual verification and tag-driven GitHub Release flow.
Tag publication requires matching Cargo versions, committed release notes,
freshness of the release inputs (see the
[dependency policy](dependency-policy.md#ci-lanes)), Foundation checks and all
native suites. The
[GHCR procedure](container-distribution.md) adds native image checks, immutable
version tags, advancing minor/latest aliases and digest reports after binary
Release creation. A configured workflow alone does not show that a given
release succeeded on every platform; check each release's workflow runs.

## License and redistribution

Copyright (c) 2026 shiroemons

Unless explicitly noted otherwise, original project code and documentation are
available under either of these licenses, at the recipient's option:

- [MIT License](../../LICENSE-MIT)
- [Apache License, Version 2.0](../../LICENSE-APACHE)

The SPDX expression is `MIT OR Apache-2.0`. A recipient chooses either license;
compliance with both at once is not required. Cargo declares this expression in
`workspace.package.license`, inherited by the application and xtask.

Third-party code, dependencies, schemas and other source material retain their own
licenses. This project's dual-license declaration does not relicense those works.
Binary archives and container distributions must include both project license
texts and the applicable third-party license and attribution notices, including
any required NOTICE content. Review the actual shipped dependency graph and
container contents when implementing distribution packaging.
