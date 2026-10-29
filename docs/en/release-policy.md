# Versioning and licensing policy

[日本語](../ja/release-policy.md)

## Product version and compatibility

gcs-mimic follows [Semantic Versioning 2.0.0](https://semver.org/), with the
additional compatibility rules below during initial development. The root
`Cargo.toml` package version is the product version and supplies `--version`.
The latest release is `0.5.0`; a manifest version alone
does not establish that a release exists. The internal xtask package is not a
separately released product.

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

`1.0.0` means a verified functional superset of the **pinned fake-gcs-server
baseline**, with a stable public gcs-mimic contract. All of these must pass:

1. Close the [full reference gate](fake-gcs-server-migration.md#delivery-and-full-superset-gate)
   for its complete capability set: configuration behavior, JSON/XML, backend
   modes, TLS, helper/event behavior, Go integration and Storage v1 gRPC.
   Test both reference backends and representative existing applications.
   Different setting names and explicit deployment/data-conversion steps are
   allowed with tested mappings; unchanged CLI spelling is not required.
2. Verify preservation of source-visible data and settings, including exposed
   history and identities, safe cutover/recovery and rollback. Missing source
   semantics cannot be waived by narrowing a comparison row.
3. Verify the named additions: independent per-bucket settings, a create-missing
   bucket initializer with API updates preserved, bidirectional directory
   synchronization, strict local authorization and V4 RSA validation, and
   persistent version history. Keep these scopes fixed in the release contract;
   unrelated future ideas do not silently become new blockers.
4. Pass the release's platform, SDK/browser, resource, persistence and public
   distribution gates. Record every baseline capability and added scope with
   executable evidence, publish migration guidance, and commit to the versioned
   public contract.

The current reference is fake-gcs-server **v1.56.1** at the pinned source revision
in the [migration guide](fake-gcs-server-migration.md). Refresh the upstream
inventory before release; explicitly record the compared version and any newer
baseline adoption. A `1.0.0` release does not assert compatibility with untested
upstream versions. Unsupported reference behavior remains a blocker, including
behavior that needs a separate legacy profile rather than native strict rules.

Full Google Cloud Storage API coverage is a separate goal, not this version's
definition. After `1.0.0`, breaking public-contract changes increment major,
compatible additions increment minor, and compatible fixes increment patch.

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
