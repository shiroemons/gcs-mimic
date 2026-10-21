# Versioning and licensing policy

## Product version and compatibility

gcs-mimic follows [Semantic Versioning 2.0.0](https://semver.org/), with the
additional compatibility rules below during initial development. The root
`Cargo.toml` package version is the product version and supplies `--version`.
The current `0.1.0` is an unreleased development baseline; a manifest version
does not establish that a release exists. The internal xtask package is not a
separately released product.

The product version is independent of Google API versions, SDK versions and the
catalog schema version. The [feature ledger](../../compatibility/features.json)
defines the supported API subset; a product version does not claim complete
Google Cloud Storage compatibility.

During `0.x` development, use these rules:

| Change | Version update | Example |
| --- | --- | --- |
| Backward-compatible bug or security fix | Patch | `0.1.0` to `0.1.1` |
| New functionality or deprecation | Minor; reset patch to zero | `0.1.1` to `0.2.0` |
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

Release `1.0.0` when the supported public contract is stable enough to maintain.
After that, breaking changes increment the major version, compatible additions
increment minor, and compatible fixes increment patch. Full Google API coverage
is not a prerequisite for `1.0.0`.

## Release identity and publication

Choose a new product version when preparing a release, not for every commit or
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

The [native distribution procedure](binary-distribution.md) defines the eight
archive targets, manual verification and future tag-driven GitHub Release flow.
Tag publication requires matching Cargo versions, committed release notes,
dependency freshness, Foundation checks and all native suites. The
[GHCR procedure](container-distribution.md) adds native image checks, immutable
version tags, advancing minor/latest aliases and digest reports after binary
Release creation. Configured workflows do not establish that a public
release or successful all-platform execution has occurred.

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
