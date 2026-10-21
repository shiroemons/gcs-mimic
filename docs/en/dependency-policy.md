# Dependency and Rust version policy

Status: project policy. Ledger tooling has a compiler pin, Cargo.lock, a dated [dependency snapshot](../../compatibility/dependency-status.json), and daily detection configuration. No server, Docker input or SDK ecosystem has been introduced. Scheduled/remote CI execution is not yet verified; per-requirement progress is recorded only in the feature ledger.

## User requirement

Use the latest stable Rust and the latest suitable non-prerelease library releases at implementation start, and keep them current throughout development. A previous planning document's example versions must never silently become the dependency baseline.

This policy applies to runtime, build and test dependencies, official SDK example projects, compiler tooling and Docker base images. English is the canonical documentation language. User-facing implementation prompts can be Japanese; code, identifiers and commits remain English.

## Latest at a defined verification time

At bootstrap, at the beginning of each implementation slice, and immediately before a release, verify official upstream release information and package registries. Record the UTC check time and actual resolved versions. A development slice uses that verified snapshot; newly arriving releases become tracked updates, rather than changing a build midway through its execution.

The research baseline on 2026-10-21 includes the official announcement of **Rust 1.99.1**, released 2026-10-15. Re-check the stable channel when implementation begins. This dated fact must not be treated as an eternal version requirement or a prediction of a future release.

Official release source: https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/

For each direct dependency, use its publisher-controlled release notes/repository and the official package registry metadata. Check package identity, stable/prerelease status, supported targets and compiler requirements. Do not infer the latest version from an old tutorial, a search snippet, or a model's memory.

## Rust toolchain, edition and supported compiler

Use the newest verified stable channel release. Nightly and beta are opt-in experimental CI lanes; they must not become a production requirement merely to use an unstable update command.

After verification, write the exact stable release number into rust-toolchain.toml, including the project's required components and targets. Record rustc -Vv, cargo -V and rustup show in build evidence. The checked-in toolchain file contains a concrete version, not a floating stable token.

Choose the newest edition supported by the selected stable toolchain and record it explicitly in Cargo.toml. Rust compiler versions and language editions are separate concepts: updating a compiler does not automatically migrate an existing crate's edition.

For this application, the default support policy is **current stable Rust only**. Set Cargo.toml rust-version to the oldest compiler that the project actually promises and verifies. Under the current-stable-only policy, advance that value with the verified toolchain baseline; do not preserve an older MSRV at the cost of holding back requested library upgrades. If a future separately published library needs a broader compiler support window, document and test that as a distinct product decision.

Sources:
- https://doc.rust-lang.org/stable/book/appendix-07-nightly-rust.html
- https://rust-lang.github.io/rustup/overrides.html
- https://doc.rust-lang.org/edition-guide/editions/
- https://doc.rust-lang.org/cargo/reference/rust-version.html

## Library selection and major-version upgrades

Select the latest non-prerelease release of each direct dependency that can satisfy the project's contract on its supported targets. A maintained 0.x release can be the correct current release; do not equate “stable selection” with “version 1.0 or greater.”

When the latest release is a breaking major, perform its required migration and relevant conformance/performance verification. Do not hide it behind an old caret range and claim the project is up to date. Review compatibility as a graph: HTTP, TLS, protobuf/gRPC, database bindings and their shared types sometimes require coordinated upgrades.

A normal cargo update changes the lockfile within the manifest's allowed requirements; it does not prove every library is at the newest major release. Update Cargo.toml constraints deliberately when adopting a breaking release. Do not depend on nightly-only cargo update --breaking for this stable-toolchain workflow.

Resolve transitive dependencies to the newest versions allowed by the selected dependency graph. If upstream requirements prevent a newer transitive major, do not force an incompatible override and call it a valid update. Prefer upgrading or replacing the direct dependency responsible, and record remaining constraints precisely.

Use only necessary crate features and dependencies. “Latest” does not mean enable every optional feature, install extra infrastructure or replace a smaller suitable dependency without a demonstrated reason.

Sources:
- https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
- https://doc.rust-lang.org/cargo/commands/cargo-update.html

## Lockfiles and reproducibility

Commit Cargo.lock for the application and generate it with Cargo. Never manually edit resolved checksums or dependency records. Commit lockfiles for SDK example applications where their package manager supports them.

Normal CI and release builds use the committed lockfile, for example cargo build --locked and cargo test --locked. A drift/update job resolves newer versions in an isolated working copy, proposes the resulting manifest/lockfile change, and runs validation.

The exact Rust toolchain, complete dependency lockfile, source commit, build settings and base-image digests together define a reproducible release input. Reproducibility does not require leaving those inputs old: update them in reviewed, tested changes and preserve the resulting history.

Source: https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html

## Daily update detection and bounded upgrade work

Current command: `python3 scripts/check-dependencies.py` (Python 3.11+). It is read-only and explicitly contacts official Rust distribution metadata, Cargo and GitHub. It discovers actual workspace registry dependencies through offline Cargo metadata, then compares resolved direct versions against all non-yanked stable releases, including new majors. Network failures fail the check. CI logs are actionable drift records; no branches or packages are published automatically. `.github/workflows/dependency-check.yaml` schedules it every calendar day with no cooldown. The command has been exercised locally; the scheduled workflow has not run. Add Docker and SDK detection when those inputs are introduced.

Run it at every slice start and before release. Review failed runs and `compatibility/dependency-status.json` exceptions at least weekly. Update toolchain, manifests, lockfiles and snapshot together after validation. There are no current freshness exceptions.

Configure daily dependency drift detection. The selected updater must cover Cargo, Rust toolchain, Docker tags/digests, GitHub Actions and the SDK example ecosystems actually present. The currently checked official Dependabot reference includes cargo, rust-toolchain, docker and other supported ecosystems; verify support again when configuring the repository.

Daily means every calendar day, including weekends, and freshness checks must not intentionally delay discovery of a new stable release. The checked Dependabot reference defines `daily` as weekdays and a default version-update cooldown of three days. If choosing Dependabot, explicitly configure an every-day cron schedule and override release delays (for example, a supported zero cooldown or equivalent immediate-detection workflow). Validate the resulting behavior; do not assume the default configuration satisfies this policy. A failing candidate remains visible until fixed or documented as an exception.

Daily checks create concrete upgrade changes or actionable drift records. They do not silently modify protected branches, mutate the build input during a test, or publish a release.

Add explicit checks at bootstrap, at slice start and before release even when daily automation exists. Use a weekly maintenance review to ensure no failed upgrade or ignored major release remains unattended; the weekly review supplements daily detection.

Patch/minor upgrades may be grouped when the groups remain reviewable. Breaking majors and toolchain/edition migrations should be independently assessable. Automatic merge, if enabled by the repository owner, is restricted to changes that satisfy the repository's actual required checks; a bot discovering a release is not evidence of compatibility.

Sources:
- https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference
- https://docs.docker.com/build/building/best-practices/

## Temporary incompatibilities must be visible

If the newest release fails required functionality, first investigate and migrate or fix the integration. If an upstream regression blocks adoption, record an explicit temporary exception containing package/current/latest version, official upstream issue or release reference, failing test evidence, impact, owner, next action and review/expiry date.

Never describe a project with an unresolved freshness exception as “all dependencies current.” Release notes and the dependency status page must surface it. Re-test the exception on new upstream releases and during daily checks; do not leave broad ignore rules indefinitely.

A real incompatibility is not a reason to fake a passing test, remove a permission check or weaken persistence semantics. A temporary pin is a documented deviation from the user's freshness target, not satisfaction of it.

## Docker base images and released artifacts

Verify the newest suitable supported builder/runtime image from its official publisher. Pin actual digests in Dockerfiles; tags alone are mutable. For a multi-architecture image, record an appropriate manifest-list/index digest or separately verified platform digests and ensure both amd64 and arm64 resolve correctly.

Do not invent a rust:<version> tag before confirming that image exists. If the official Rust image has not yet published the new compiler, use a verified supported builder and install the exact official toolchain through a documented process, or record a visible publication-lag exception. Do not silently use the previous compiler.

The update workflow checks tag changes and replacement digests and submits tested updates. Rebuild released runtime images when accepted base-image updates change them; merely updating a Dockerfile does not update an already published image.

Use immutable release version tags and publish the resolved image digest in release metadata. A convenience latest tag may exist after releases are actually published, but installation and reproduction examples should also show an explicit release/digest. Before the first release, use a locally built lowercase gcs-mimic:dev image.

Source: https://docs.docker.com/build/building/best-practices/

## CI lanes

| Lane | Toolchain/dependencies | Purpose |
| --- | --- | --- |
| Required PR/release | Exact pinned stable, committed lockfiles and digests | Reproduce the release inputs and validate supported targets. |
| Current stable compatibility | Freshly updated stable toolchain, committed lockfile | Detect compiler drift immediately; passing alone does not update the committed baseline. |
| Latest-dependency candidate | Verified current stable, proposed manifests/lockfiles | Exercise dependency upgrades, including deliberate major migrations. |
| Optional experimental | Explicit beta/nightly | Early warning only; never substitutes for passing stable checks. |

Release gates require a successful check against the currently verified stable compiler, adoption of the latest direct dependencies or explicit visible exceptions, successful target-platform container smoke tests, and recorded version provenance.

## Dependency status evidence

Maintain a machine-readable dependency status record containing checked_at_utc, toolchain channel/exact version, edition, declared rust-version, package current/latest version, source URL, resolution constraints, Docker digest per platform and any exception reference. Store actual command/test evidence with the change.

Report the difference between “no update found,” “update available,” “candidate tests failing,” and “update merged.” Never mark a dependency current merely because the scheduled job ran.

## Definition of done

All policy acceptance rows begin NOT_STARTED. They become implemented only after the corresponding files, update workflow, CI lanes and evidence exist and have been executed. This research document itself is not evidence that the project has a current compiler, latest crates, committed lockfiles or a running daily update job.
