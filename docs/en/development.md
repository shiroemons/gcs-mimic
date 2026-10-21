# Development and verification

Requires rustup and curl for the explicit online freshness check. Python is not required. Rust 1.99.1 is the verified latest stable compiler; edition 2024 is explicit. No Google credentials or project are needed.

```sh
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo +1.99.1 fetch --locked
cargo +1.99.1 xtask dependencies check
cargo +1.99.1 xtask compatibility validate
cargo +1.99.1 xtask compatibility render
cargo +1.99.1 xtask compatibility check
cargo +1.99.1 test --workspace --locked --offline
cargo +1.99.1 fmt --all --check
cargo +1.99.1 clippy --workspace --all-targets --locked --offline -- -D warnings
bash scripts/smoke-containers.sh
```

The xtask alias uses locked offline resolution. Fetch all locked packages first. Explicit `+1.99.1` overrides a conflicting `RUSTUP_TOOLCHAIN` environment variable.

Latest verified direct releases: Tokio 1.54.0, Hyper 1.12.0, hyper-util 0.1.21, http-body-util 0.1.5 and serde_json 1.0.154. Versions, licenses, sizes and sources are in the [dependency snapshot](../../compatibility/dependency-status.json). Cargo.lock records the full graph. Docker inputs use actual multiarch digests.

`dependencies check` contacts official Rust, Cargo, GitHub, Debian and Docker metadata. Other Rust tests are offline and use only loopback. All direct dependencies are compared against non-yanked stable releases, including new majors; Docker tags/digests and Debian stable codename are checked separately. Exit 1 means updates; exit 2 means verification failed. Run at slice start and before release and review exceptions weekly. No manifests are modified and nothing is published automatically.

Pinned/current-stable Rust CI and native amd64/arm64 container CI are configured. Daily freshness runs at 03:17 UTC every calendar day without cooldown after reaching the default branch. Remote CI/schedule execution remains unverified. Latest-dependency candidate builds and upgrade submission are not implemented. Local arm64 runs natively; local amd64 uses emulation.

`compatibility/features.json` is the only implementation-status authority. English and Japanese views are generated together; Japanese supplementary material remains under [planning/](../../planning/README.md). Ledger tooling validates evidence structure and links, not the truth of submitted logs. No GCS behavior is claimed by internal probes.

- [Design and extension boundaries](design.md)
- [Roadmap](roadmap.md)
- [Compatibility policy](compatibility-policy.md)
- [Generated English matrix](feature-matrix.md)
- [API inventory](api-inventory.md)
- [Sources and gaps](sources.md)
- [Dependency policy](dependency-policy.md)
- [Future onboarding contract](getting-started-spec.md)
- [Tests](evidence/p0-tests.txt)
- [Container checks](evidence/p0-containers.txt)
- [Freshness check](evidence/p0-dependencies.txt)
- [Coding-agent rules](../../AGENTS.md)
